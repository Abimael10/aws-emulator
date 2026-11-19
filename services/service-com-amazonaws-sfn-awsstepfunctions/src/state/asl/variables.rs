// SPDX-License-Identifier: CC0-1.0

//! The variables one walk has ASSIGNED, as the JSONPath half of the language reads
//! them.
//!
//! One concern in a file of its own, reached from [`super`]: the other query language
//! binds the variables it reads into the expression it evaluates
//! ([`super::super::jsonata::Bindings`]), while a JSONPath reference reads one as the
//! FIRST TERM of a path (`$total`, `$total.items[0]`) — and every path this emulator
//! evaluates is read by the plain functions of [`super::jsonpath`], which the walk
//! reaches through several layers (a payload template's member, a `Choice` rule's
//! comparator, a `*Path` member of a state). So the scope is AMBIENT to the thread the
//! walk runs on, exactly as the work that walk may do and the documents it was given
//! are ([`super::WorkAllowance`], [`super::GivenDocuments`]).

use std::cell::RefCell;

use super::*;

thread_local! {
    /// The variables in scope for the state the walk running on THIS thread is
    /// evaluating.
    ///
    /// Empty on a thread no walk installed a scope on: the same primitives also
    /// validate a definition at create time, which assigns nothing.
    static SCOPE: RefCell<BTreeMap<String, Value>> = const { RefCell::new(BTreeMap::new()) };
}

/// The variables in scope for ONE state's turn, installed on the thread that turn runs
/// on for as long as it lasts.
///
/// The scope in force when it was installed is restored when it is dropped — on the
/// error paths and while unwinding as much as at the end of a turn — so a state nested
/// inside another (a `Map` iteration, a `Parallel` branch) cannot leave the enclosing
/// state reading what it assigned, which is how the language scopes an `Assign`.
#[derive(Debug)]
pub(super) struct VariableScope {
    /// What was in scope for the turn this one interrupted.
    restored: BTreeMap<String, Value>,
}

impl VariableScope {
    /// Install `variables` as the scope the state about to run on this thread reads.
    pub(super) fn of(variables: &BTreeMap<String, Value>) -> Self {
        Self {
            restored: SCOPE.with(|scope| scope.replace(variables.clone())),
        }
    }

    /// Install `variables` as the scope from here on, for a state whose own `Assign`
    /// has just written to it: "a variable this state assigns is in scope for the
    /// state's own expressions", so what the state evaluates after its assignments
    /// reads them.
    pub(super) fn reinstall(variables: &BTreeMap<String, Value>) {
        SCOPE.with(|scope| scope.replace(variables.clone()));
    }

    /// Bind ONE variable in the scope in force, for the member of an `Assign` block
    /// that has just been built.
    ///
    /// The members of a block are evaluated in order and each reads the ones before it,
    /// so the scope changes once per member: binding the one variable costs that one
    /// value rather than a copy of the whole scope, which is what keeps the cost of a
    /// block the SUM of its members and not the product of their count and the scope's
    /// size.
    pub(super) fn bind(name: &str, value: &Value) {
        SCOPE.with(|scope| {
            scope
                .borrow_mut()
                .insert(name.to_string(), value.clone())
        });
    }
}

impl Drop for VariableScope {
    fn drop(&mut self) {
        let restored = std::mem::take(&mut self.restored);
        SCOPE.with(|scope| scope.replace(restored));
    }
}

/// What the variable `name` holds in the scope of the walk running on this thread, or
/// `None` where nothing reaching this state assigned it.
pub(super) fn scoped_variable(name: &str) -> Option<Value> {
    SCOPE.with(|scope| scope.borrow().get(name).cloned())
}

/// The VARIABLE a JSONPath reference names and the rest of the path into it, for a
/// path that names one: `$total` is the whole value of `total`, and `$total.items[0]`
/// is a place inside it.
///
/// A path names a variable when its first term is `$` followed by an identifier, which
/// is how both of the language's query languages spell a variable reference
/// ([`super::variable_references`], the report `DescribeStateMachine` answers with):
/// `$` alone and `$.member` are the state's own data, and `$$` is the context object.
pub(super) fn variable_reference(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix('$')?;
    let first = rest.as_bytes().first().copied()?;
    if !first.is_ascii_alphabetic() && first != b'_' {
        return None;
    }
    let end = rest
        .as_bytes()
        .iter()
        .position(|byte| !is_member_byte(*byte))
        .unwrap_or(rest.len());
    Some(rest.split_at(end))
}
