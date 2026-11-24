// SPDX-License-Identifier: CC0-1.0

//! Planning one statement's JOIN: normalising an equality that constrains two
//! relations so the engine can resolve it by a KEYED LOOKUP instead of by
//! enumerating their cross product.
//!
//! A data warehouse answers an analytic join of two large relations in seconds,
//! because an equality between them bounds the pairs it has to look at: it
//! builds a key over one relation and probes it once per row of the other. An
//! engine can only do that when each side of the equality names ONE relation —
//! `b.n = -1 - a.n` is a key probe of `b`, while the same constraint written
//! `a.n + b.n = -1` names both relations on one side and leaves the engine with
//! nothing to key on, so it compares every pair of rows and a join of two
//! eighty-thousand-row relations becomes six billion comparisons.
//!
//! So an equality that mixes the relations is TRANSPOSED here, which is the
//! ordinary algebra of an equation: the additive terms of one relation are
//! collected on the left and everything else moved to the right, with its sign
//! flipped. The statement runs with the same meaning and the engine plans it as
//! the keyed join it is.
//!
//! Applied where it is EXACT and nowhere else. The terms it moves are integer
//! literals and columns the relations declare with one of the warehouse's exact
//! integer types, for which `x + y = z` and `x = z - y` admit exactly the same
//! rows; a term of any other shape — a floating-point column, whose arithmetic
//! rounds, a function call, a cast, a column no relation of the statement
//! declares — leaves the equality exactly as the caller wrote it. So does an
//! equality that already names one relation per side, which the engine can
//! already key on: nothing is rewritten unless the rewrite is what lets the
//! engine key the join.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::ops::ControlFlow;

use sqlparser::ast::{
    BinaryOperator, Expr, Query, SetExpr, Statement, TableFactor, TableWithJoins, UnaryOperator,
    VisitMut, VisitorMut,
};

use super::copy_load::TypeClass;

/// Whether a column declared with this type is stored as an EXACT INTEGER,
/// which is what the transposition here is exact over.
///
/// The names are the warehouse's own integer types and their documented aliases,
/// read through the one classification of a declared type this crate keeps
/// ([`super::copy_load::TypeClass`]). A column that declares no type at all
/// takes any value, so it is not one of them.
pub fn declares_integer(declared: &str) -> bool {
    !declared.trim().is_empty() && TypeClass::of(declared) == TypeClass::Integer
}

/// What the engine declares about the columns of the relations a statement
/// names, which is what decides whether transposing a term across an equality
/// is exact.
pub trait DeclaredColumns {
    /// Whether the relation held under the engine name `table` declares
    /// `column` with one of the warehouse's exact integer types.
    fn is_integer(&self, table: &str, column: &str) -> bool;
}

/// The engine's own catalog, asked once per relation.
///
/// The declared type of each column is read off the query that returns them,
/// the way every other reader of a relation's columns in this crate reads them;
/// a relation the engine cannot prepare a read of declares nothing.
pub struct EngineColumns<'a> {
    connection: &'a rusqlite::Connection,
    /// The integer-typed columns of each relation already asked about, by
    /// lower-case relation name.
    integer: RefCell<HashMap<String, BTreeSet<String>>>,
}

impl<'a> EngineColumns<'a> {
    /// Read the declarations of this connection's relations.
    pub fn new(connection: &'a rusqlite::Connection) -> Self {
        Self {
            connection,
            integer: RefCell::new(HashMap::new()),
        }
    }

    /// Read the integer-typed columns of one relation off the engine, unless
    /// this relation has already been asked about.
    fn read(&self, relation: &str, table: &str) {
        if self.integer.borrow().contains_key(relation) {
            return;
        }
        let quoted = table.replace('"', "\"\"");
        let mut columns = BTreeSet::new();
        if let Ok(probe) = self
            .connection
            .prepare(&format!("SELECT * FROM \"{quoted}\" LIMIT 0"))
        {
            for column in probe.columns() {
                if declares_integer(column.decl_type().unwrap_or_default()) {
                    columns.insert(column.name().to_ascii_lowercase());
                }
            }
        }
        let mut held = self.integer.borrow_mut();
        // Bounded like every other map this service fills on request: a
        // statement naming more relations than this plans no further ones.
        if held.len() < MAX_PLANNED_RELATIONS {
            held.insert(relation.to_string(), columns);
        }
    }
}

/// How many relations ONE connection's declarations are remembered for while a
/// statement is planned.
const MAX_PLANNED_RELATIONS: usize = 1_024;

impl DeclaredColumns for EngineColumns<'_> {
    fn is_integer(&self, table: &str, column: &str) -> bool {
        let relation = table.to_ascii_lowercase();
        self.read(&relation, table);
        let column = column.to_ascii_lowercase();
        self.integer
            .borrow()
            .get(&relation)
            .is_some_and(|columns| columns.contains(&column))
    }
}

/// Plan the joins of `statement`, and report whether anything was transposed.
///
/// Every query of the statement is planned, including the ones nested inside it:
/// a subquery joins its own relations and is keyed the same way.
pub fn planned(statement: &mut Statement, columns: &dyn DeclaredColumns) -> bool {
    let mut planner = Planner {
        columns,
        scopes: Vec::new(),
        changed: false,
    };
    let _walked = statement.visit(&mut planner);
    planner.changed
}

/// The relations one query reads, by the name the query refers to each of them
/// by: the alias it gave the relation, or the relation's own name.
type Scope = HashMap<String, String>;

/// The transposition, over the query it is planning.
struct Planner<'a> {
    /// What the engine declares about the relations' columns.
    columns: &'a dyn DeclaredColumns,
    /// The relations of each query being walked, innermost last — a correlated
    /// subquery's own relations are what its expressions are read against.
    scopes: Vec<Scope>,
    /// Whether an equality was really transposed, which is what decides whether
    /// the statement is re-rendered at all.
    changed: bool,
}

impl VisitorMut for Planner<'_> {
    type Break = ();

    fn pre_visit_query(&mut self, query: &mut Query) -> ControlFlow<Self::Break> {
        self.scopes.push(match query.body.as_ref() {
            SetExpr::Select(select) => scope_of(&select.from),
            _ => Scope::new(),
        });
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &mut Query) -> ControlFlow<Self::Break> {
        self.scopes.pop();
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<Self::Break> {
        let Some(scope) = self.scopes.last() else {
            return ControlFlow::Continue(());
        };
        // Only a join of two relations has a cross product to avoid.
        if scope.len() < 2 {
            return ControlFlow::Continue(());
        }
        if let Some(transposed) = transposed(expr, scope, self.columns) {
            *expr = transposed;
            self.changed = true;
        }
        ControlFlow::Continue(())
    }
}

/// The relations one query's `FROM` reads, by the name it refers to each of them
/// by.
///
/// Only a relation the engine holds under a name of its own is collected: a
/// derived table and a table function declare no column this planning can read,
/// so a term qualified by one of them is left alone.
fn scope_of(from: &[TableWithJoins]) -> Scope {
    let mut scope = Scope::new();
    for table in from {
        collect(&table.relation, MAX_WALK_DEPTH, &mut scope);
        for join in &table.joins {
            collect(&join.relation, MAX_WALK_DEPTH, &mut scope);
        }
    }
    scope
}

/// How deep into ONE expression or ONE nest of joins any walk below descends.
///
/// The depth is the caller's to choose — a statement may spell an arithmetic
/// chain or a nest of joins as deeply as its text allows — so every walk here
/// carries this budget and stops at it rather than recursing as far as the
/// statement goes: a walk that ran out of budget leaves the equality exactly as
/// written, which is what every other case this planning declines does. Well
/// above the depth a join predicate is written at, and far below what this
/// process has stack for.
const MAX_WALK_DEPTH: usize = 64;

/// Collect one table factor of a `FROM` into `scope`, descending at most `depth`
/// nested joins.
fn collect(factor: &TableFactor, depth: usize, scope: &mut Scope) {
    let Some(depth) = depth.checked_sub(1) else {
        return;
    };
    match factor {
        TableFactor::Table { name, alias, .. } => {
            let [part] = name.0.as_slice() else {
                return;
            };
            let Some(table) = part.as_ident().map(|ident| ident.value.clone()) else {
                return;
            };
            let referred_to_as = alias
                .as_ref()
                .map_or_else(|| table.clone(), |alias| alias.name.value.clone());
            scope.insert(referred_to_as.to_ascii_lowercase(), table);
        }
        TableFactor::NestedJoin {
            table_with_joins, ..
        } => {
            collect(&table_with_joins.relation, depth, scope);
            for join in &table_with_joins.joins {
                collect(&join.relation, depth, scope);
            }
        }
        _ => {}
    }
}

/// One additive term of an equality, and the sign it carries once the equality
/// is read as a sum that equals zero.
#[derive(Debug, Clone)]
struct Term {
    /// Whether the term is added or subtracted.
    positive: bool,
    /// The relation the term reads a column of, or `None` for a literal.
    relation: Option<String>,
    /// Whether the term is a bare column of its relation, which is what an
    /// engine can key a lookup on.
    bare_column: bool,
    /// The term itself.
    expr: Expr,
}

/// The same equality with each side naming at most one relation, or `None` when
/// this expression is not an equality that mixes two relations exactly.
fn transposed(expr: &Expr, scope: &Scope, columns: &dyn DeclaredColumns) -> Option<Expr> {
    let Expr::BinaryOp {
        left,
        op: BinaryOperator::Eq,
        right,
    } = expr
    else {
        return None;
    };
    // An equality one side of which is a bare column of a relation the other
    // side does not read is one the engine can already key on as written.
    if already_keyed(left, right, scope) {
        return None;
    }
    let mut terms = Vec::new();
    // Read as `left - right = 0`, so every term of the equality carries one
    // sign and moving it across the equality is flipping that sign.
    flatten(left, true, scope, columns, MAX_WALK_DEPTH, &mut terms)?;
    flatten(right, false, scope, columns, MAX_WALK_DEPTH, &mut terms)?;
    let named: BTreeSet<&str> = terms
        .iter()
        .filter_map(|term| term.relation.as_deref())
        .collect();
    // Two relations: with one there is nothing to separate, and with three no
    // arrangement of the terms gives each side a single relation.
    if named.len() != 2 {
        return None;
    }
    // The relation the lookup is keyed on has to be left ALONE on its side: one
    // bare column of it, which is what an engine can probe an index of.
    let keyed = named.into_iter().find(|relation| {
        let mut terms = terms
            .iter()
            .filter(|term| term.relation.as_deref() == Some(*relation));
        terms
            .next()
            .is_some_and(|term| term.positive && term.bare_column)
            && terms.next().is_none()
    })?;
    let key = terms
        .iter()
        .find(|term| term.relation.as_deref() == Some(keyed))?
        .expr
        .clone();
    // Everything else moves to the other side, which flips its sign.
    let rest: Vec<Term> = terms
        .iter()
        .filter(|term| term.relation.as_deref() != Some(keyed))
        .map(|term| Term {
            positive: !term.positive,
            ..term.clone()
        })
        .collect();
    Some(Expr::BinaryOp {
        left: Box::new(key),
        op: BinaryOperator::Eq,
        right: Box::new(sum(&rest)?),
    })
}

/// Whether the engine can key this equality as the caller wrote it: one of its
/// sides is a bare column, and the other side reads nothing of that column's own
/// relation — which is the constraint an engine probes an index of the relation
/// with.
fn already_keyed(left: &Expr, right: &Expr, scope: &Scope) -> bool {
    keyed_on(left, right, scope) || keyed_on(right, left, scope)
}

/// Whether `side` is the key of the lookup and `other` the expression it is
/// probed with.
fn keyed_on(side: &Expr, other: &Expr, scope: &Scope) -> bool {
    let Some(relation) = bare_column(side, MAX_WALK_DEPTH, scope) else {
        return false;
    };
    !relations_of(other, scope).contains(&relation)
}

/// The relation a bare column reference reads, as the query refers to it, or
/// `None` for an expression that is not a bare column.
///
/// A column this cannot attribute to one relation of the query — an unqualified
/// one, or one qualified by a relation the query's `FROM` does not name — reads
/// the empty name, which no relation of the query answers to: an equality
/// carrying it is left exactly as written. So does an expression nested deeper
/// than this walk descends ([`MAX_WALK_DEPTH`]).
fn bare_column(expr: &Expr, depth: usize, scope: &Scope) -> Option<String> {
    let depth = depth.checked_sub(1)?;
    match expr {
        Expr::Nested(inner) => bare_column(inner, depth, scope),
        Expr::Identifier(_) => Some(String::new()),
        Expr::CompoundIdentifier(parts) => {
            let [qualifier, _column] = parts.as_slice() else {
                return Some(String::new());
            };
            let qualifier = qualifier.value.to_ascii_lowercase();
            Some(if scope.contains_key(&qualifier) {
                qualifier
            } else {
                String::new()
            })
        }
        _ => None,
    }
}

/// The relations an expression reads columns of, as the query refers to them.
///
/// A column no relation of the query declares under a name of its own — an
/// unqualified one, which more than one relation of a join may declare, or one
/// qualified by a derived table — reads a relation this cannot name, and is
/// reported under the empty name so an equality carrying it is never read as
/// naming one relation. So does a subexpression nested deeper than this walk
/// descends ([`MAX_WALK_DEPTH`]).
fn relations_of(expr: &Expr, scope: &Scope) -> BTreeSet<String> {
    let mut named = BTreeSet::new();
    walk_columns(expr, MAX_WALK_DEPTH, scope, &mut named);
    named
}

/// Collect the relations `expr` reads columns of into `named`, descending at
/// most `depth` levels into it.
fn walk_columns(expr: &Expr, depth: usize, scope: &Scope, named: &mut BTreeSet<String>) {
    let Some(depth) = depth.checked_sub(1) else {
        // Deeper than this walk goes: what it reads is a relation this cannot
        // name.
        named.insert(String::new());
        return;
    };
    match expr {
        Expr::CompoundIdentifier(parts) => {
            let qualifier = parts
                .first()
                .map(|part| part.value.to_ascii_lowercase())
                .filter(|qualifier| parts.len() == 2 && scope.contains_key(qualifier));
            named.insert(qualifier.unwrap_or_default());
        }
        Expr::Identifier(_) => {
            named.insert(String::new());
        }
        Expr::Nested(inner) => walk_columns(inner, depth, scope, named),
        Expr::UnaryOp { expr, .. } => walk_columns(expr, depth, scope, named),
        Expr::BinaryOp { left, right, .. } => {
            walk_columns(left, depth, scope, named);
            walk_columns(right, depth, scope, named);
        }
        Expr::Cast { expr, .. } => walk_columns(expr, depth, scope, named),
        Expr::Value(_) => {}
        // Anything else may read a relation this walk cannot attribute, so it
        // is reported as naming one it cannot name.
        _ => {
            named.insert(String::new());
        }
    }
}

/// Read `expr` as a sum of terms, each carrying `positive` as flipped by the
/// subtractions above it, and collect them into `terms`.
///
/// `None` as soon as a term is one transposing would not be exact over, which
/// is what leaves the equality as the caller wrote it.
fn flatten(
    expr: &Expr,
    positive: bool,
    scope: &Scope,
    columns: &dyn DeclaredColumns,
    depth: usize,
    terms: &mut Vec<Term>,
) -> Option<()> {
    // Bounded twice, because a caller chooses both: how many terms the sum may
    // carry, and how deep into the expression this descends for them
    // ([`MAX_WALK_DEPTH`]) — an arithmetic chain the statement spells longer
    // than either bound is left exactly as written.
    let depth = depth.checked_sub(1)?;
    if terms.len() >= MAX_TERMS {
        return None;
    }
    match expr {
        Expr::Nested(inner) => flatten(inner, positive, scope, columns, depth, terms),
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } => flatten(expr, !positive, scope, columns, depth, terms),
        Expr::UnaryOp {
            op: UnaryOperator::Plus,
            expr,
        } => flatten(expr, positive, scope, columns, depth, terms),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Plus,
            right,
        } => {
            flatten(left, positive, scope, columns, depth, terms)?;
            flatten(right, positive, scope, columns, depth, terms)
        }
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Minus,
            right,
        } => {
            flatten(left, positive, scope, columns, depth, terms)?;
            flatten(right, !positive, scope, columns, depth, terms)
        }
        _ => {
            terms.push(term(expr, positive, scope, columns)?);
            Some(())
        }
    }
}

/// How many additive terms one equality may be read as. An equality carrying
/// more is left as written.
const MAX_TERMS: usize = 32;

/// One leaf of the sum: an integer literal, or a column one relation of the
/// query declares with an exact integer type.
fn term(
    expr: &Expr,
    positive: bool,
    scope: &Scope,
    columns: &dyn DeclaredColumns,
) -> Option<Term> {
    match expr {
        Expr::Value(value) => {
            let sqlparser::ast::Value::Number(number, _) = &value.value else {
                return None;
            };
            // An exact integer: a literal carrying a decimal point or an
            // exponent is a value whose arithmetic rounds.
            number.parse::<i64>().ok()?;
            Some(Term {
                positive,
                relation: None,
                bare_column: false,
                expr: expr.clone(),
            })
        }
        Expr::CompoundIdentifier(parts) => {
            let [qualifier, column] = parts.as_slice() else {
                return None;
            };
            let qualifier = qualifier.value.to_ascii_lowercase();
            let table = scope.get(&qualifier)?;
            if !columns.is_integer(table, &column.value) {
                return None;
            }
            Some(Term {
                positive,
                relation: Some(qualifier),
                bare_column: true,
                expr: expr.clone(),
            })
        }
        _ => None,
    }
}

/// The terms added up, in the order they were read: the first one carries its
/// own sign, and each one after it is added or subtracted.
fn sum(terms: &[Term]) -> Option<Expr> {
    let (first, rest) = terms.split_first()?;
    let mut total = if first.positive {
        first.expr.clone()
    } else {
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr: Box::new(first.expr.clone()),
        }
    };
    for term in rest {
        total = Expr::BinaryOp {
            left: Box::new(total),
            op: if term.positive {
                BinaryOperator::Plus
            } else {
                BinaryOperator::Minus
            },
            right: Box::new(term.expr.clone()),
        };
    }
    Some(total)
}
