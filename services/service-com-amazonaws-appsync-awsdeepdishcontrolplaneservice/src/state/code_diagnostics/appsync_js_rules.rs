// SPDX-License-Identifier: CC0-1.0

//! The APPSYNC_JS rule set a definition's `code` is checked against once it compiles.
//!
//! The APPSYNC_JS runtime runs a restricted JavaScript: a construct the runtime does not
//! implement is refused as the definition is WRITTEN rather than failing when a field is
//! resolved, and the refusal names the rule the code broke. The rules are the published
//! APPSYNC_JS ones, each named the way a diagnostic names it —
//! `@aws-appsync/<rule>` ([`RULE_SET`]) — and each stating that the construct it is about is
//! not supported; recorded behaviour shows two of them (`no-for` over a `for` statement and
//! `no-disallowed-unary-operators` over `++`), with the position AND the length of the
//! construct the rule is about.
//!
//! One pass over the module the compile built, so the rule is checked over the SYNTAX TREE
//! rather than over the code's text: a `for` that is the name of a property, a `this` inside
//! a string and an `await` in a comment are not constructs the code uses, and the length each
//! diagnostic carries is the construct's own.

use ::swc_common::Span;
use ::swc_ecma_ast::{
    ArrowExpr, AwaitExpr, Class, ContinueStmt, DoWhileStmt, Expr, ForInStmt, ForOfStmt, ForStmt,
    Function, Ident, LabeledStmt, Module, Pat, Regex, Stmt, ThisExpr, TryStmt, UpdateExpr,
    UpdateOp, WhileStmt, YieldExpr,
};
use ::swc_ecma_visit::{Visit, VisitWith};

use crate::ops::evaluate_code::appsync_js::CodeFault;

/// How every rule of the set is named: the rule set's own prefix, which a diagnostic states
/// before the rule's name.
pub const RULE_SET_PREFIX: &str = "@aws-appsync";

/// A loop: "For statements are not supported."
const NO_FOR: &str = "@aws-appsync/no-for";
/// See [`NO_FOR`].
const NO_WHILE: &str = "@aws-appsync/no-while";
/// Jumping out of a step of a loop.
const NO_CONTINUE: &str = "@aws-appsync/no-continue";
/// A statement reached by a label.
const NO_LABELS: &str = "@aws-appsync/no-labels";
/// Catching what a step raised.
const NO_TRY: &str = "@aws-appsync/no-try";
/// A class.
const NO_CLASSES: &str = "@aws-appsync/no-classes";
/// A function that does not run to its end in one go, and the expression that waits for one.
const NO_ASYNC: &str = "@aws-appsync/no-async";
/// See [`NO_ASYNC`].
const NO_AWAIT: &str = "@aws-appsync/no-await";
/// A function that yields, and the expression that yields from one.
const NO_GENERATORS: &str = "@aws-appsync/no-generators";
/// See [`NO_GENERATORS`].
const NO_YIELD: &str = "@aws-appsync/no-yield";
/// A regular expression literal.
const NO_REGEX: &str = "@aws-appsync/no-regex";
/// The receiver of the call.
const NO_THIS: &str = "@aws-appsync/no-this";
/// The value a step would wait on.
const NO_PROMISE: &str = "@aws-appsync/no-promise";
/// An operator that both reads and writes what it is applied to.
const NO_DISALLOWED_UNARY_OPERATORS: &str = "@aws-appsync/no-disallowed-unary-operators";

/// Every rule this pass checks, by the name a diagnostic states.
///
/// Each name is `<the set's prefix>/<the construct it forbids>`
/// ([`RULE_SET_PREFIX`]), and a definition is refused on the first pass that finds any of
/// them broken.
pub const RULE_SET: &[&str] = &[
    NO_ASYNC,
    NO_AWAIT,
    NO_CLASSES,
    NO_CONTINUE,
    NO_DISALLOWED_UNARY_OPERATORS,
    NO_FOR,
    NO_GENERATORS,
    NO_LABELS,
    NO_PROMISE,
    NO_REGEX,
    NO_THIS,
    NO_TRY,
    NO_WHILE,
    NO_YIELD,
];

/// The sentence a rule about STATEMENTS of one kind states, naming the kind.
fn statements_sentence(kind: &str) -> String {
    format!("{kind} statements are not supported.")
}

/// The name the value a step would wait on is reached under, which is what the rule about it
/// is written over.
const PROMISE: &str = "Promise";

/// How little of the walk's own stack may be left before the next level of it is taken on the
/// HEAP instead, and how much is taken at a time ([`WALK_STACK_GROWTH`]).
///
/// The walk goes as deep as the code nests, which caller-supplied code chooses, so its depth
/// is not a demand on the thread's stack at all: each level checks what is left and moves
/// onto the heap before it runs out. A level takes far less than the red zone, so the check
/// is always made with room to make it.
const WALK_RED_ZONE: usize = 128 * 1024;
/// See [`WALK_RED_ZONE`].
const WALK_STACK_GROWTH: usize = 2 * 1024 * 1024;

/// Every rule `module` breaks, in the order the constructs they are about sit in the code.
pub(super) fn rule_faults(code: &str, module: &Module) -> Vec<CodeFault> {
    let mut checked = RuleSet {
        code,
        faults: Vec::new(),
    };
    module.visit_with(&mut checked);
    let mut faults = checked.faults;
    faults.sort_by_key(|fault| (fault.line, fault.column));
    faults
}

/// One pass of the rule set over one module.
struct RuleSet<'a> {
    /// The code the module was built from, which every position and length is counted in.
    code: &'a str,
    /// The rules the pass has found broken so far.
    faults: Vec<CodeFault>,
}

impl RuleSet<'_> {
    /// Report `rule` as broken by the construct at `span`.
    fn broke(&mut self, rule: &str, sentence: &str, span: Span) {
        let stretch = super::offset(span.lo)..super::offset(span.hi);
        self.faults
            .push(super::rule_fault(self.code, rule, sentence, stretch));
    }
}

impl Visit for RuleSet<'_> {
    // The nodes a definition nests THROUGH, and so the levels this walk recurses at: an
    // expression inside an expression, a statement inside a statement, and a binding pattern
    // inside a binding pattern (an array or object pattern holds patterns directly, reaching
    // neither of the other two). Each takes the next level onto the HEAP when the thread's own
    // stack runs low ([`WALK_RED_ZONE`]), so code that nests as deeply as the modelled maximum
    // length of `code` allows is walked to the end rather than spending the stack. Every other
    // node of this pass is reached through one of these three.
    fn visit_expr(&mut self, node: &Expr) {
        ::stacker::maybe_grow(WALK_RED_ZONE, WALK_STACK_GROWTH, || {
            node.visit_children_with(self)
        });
    }

    fn visit_stmt(&mut self, node: &Stmt) {
        ::stacker::maybe_grow(WALK_RED_ZONE, WALK_STACK_GROWTH, || {
            node.visit_children_with(self)
        });
    }

    fn visit_pat(&mut self, node: &Pat) {
        ::stacker::maybe_grow(WALK_RED_ZONE, WALK_STACK_GROWTH, || {
            node.visit_children_with(self)
        });
    }

    fn visit_for_stmt(&mut self, node: &ForStmt) {
        self.broke(NO_FOR, &statements_sentence("For"), node.span);
        node.visit_children_with(self);
    }

    fn visit_for_in_stmt(&mut self, node: &ForInStmt) {
        self.broke(NO_FOR, &statements_sentence("For"), node.span);
        node.visit_children_with(self);
    }

    fn visit_for_of_stmt(&mut self, node: &ForOfStmt) {
        self.broke(NO_FOR, &statements_sentence("For"), node.span);
        node.visit_children_with(self);
    }

    fn visit_while_stmt(&mut self, node: &WhileStmt) {
        self.broke(NO_WHILE, &statements_sentence("While"), node.span);
        node.visit_children_with(self);
    }

    fn visit_do_while_stmt(&mut self, node: &DoWhileStmt) {
        self.broke(NO_WHILE, &statements_sentence("While"), node.span);
        node.visit_children_with(self);
    }

    fn visit_continue_stmt(&mut self, node: &ContinueStmt) {
        self.broke(NO_CONTINUE, &statements_sentence("Continue"), node.span);
        node.visit_children_with(self);
    }

    fn visit_labeled_stmt(&mut self, node: &LabeledStmt) {
        self.broke(NO_LABELS, &statements_sentence("Labeled"), node.span);
        node.visit_children_with(self);
    }

    fn visit_try_stmt(&mut self, node: &TryStmt) {
        self.broke(NO_TRY, &statements_sentence("Try"), node.span);
        node.visit_children_with(self);
    }

    fn visit_class(&mut self, node: &Class) {
        self.broke(NO_CLASSES, "Classes are not supported.", node.span);
        node.visit_children_with(self);
    }

    fn visit_function(&mut self, node: &Function) {
        if node.is_async {
            self.broke(NO_ASYNC, "Async functions are not supported.", node.span);
        }
        if node.is_generator {
            self.broke(
                NO_GENERATORS,
                "Generator functions are not supported.",
                node.span,
            );
        }
        node.visit_children_with(self);
    }

    fn visit_arrow_expr(&mut self, node: &ArrowExpr) {
        if node.is_async {
            self.broke(NO_ASYNC, "Async functions are not supported.", node.span);
        }
        if node.is_generator {
            self.broke(
                NO_GENERATORS,
                "Generator functions are not supported.",
                node.span,
            );
        }
        node.visit_children_with(self);
    }

    fn visit_await_expr(&mut self, node: &AwaitExpr) {
        self.broke(NO_AWAIT, "Await expressions are not supported.", node.span);
        node.visit_children_with(self);
    }

    fn visit_yield_expr(&mut self, node: &YieldExpr) {
        self.broke(NO_YIELD, "Yield expressions are not supported.", node.span);
        node.visit_children_with(self);
    }

    fn visit_regex(&mut self, node: &Regex) {
        self.broke(
            NO_REGEX,
            "Regular expressions are not supported.",
            node.span,
        );
        node.visit_children_with(self);
    }

    fn visit_this_expr(&mut self, node: &ThisExpr) {
        self.broke(NO_THIS, "The this keyword is not supported.", node.span);
        node.visit_children_with(self);
    }

    fn visit_ident(&mut self, node: &Ident) {
        // The name is only reached as an identifier here: a member or a property of that
        // name is a different node, so an object with a `Promise` member breaks no rule. The
        // rule is over the NAME rather than over what it resolves to, so code that declares a
        // binding of its own under that name is reported too — the runtime serves no promise
        // under it either way.
        if &*node.sym == PROMISE {
            self.broke(NO_PROMISE, "Promises are not supported.", node.span);
        }
        node.visit_children_with(self);
    }

    fn visit_update_expr(&mut self, node: &UpdateExpr) {
        let operator = match node.op {
            UpdateOp::PlusPlus => "++",
            UpdateOp::MinusMinus => "--",
        };
        self.broke(
            NO_DISALLOWED_UNARY_OPERATORS,
            // The one sentence of the set that names what it is about rather than stating a
            // kind, and the one that states no full stop, as recorded.
            &format!("The operator {operator} is not supported"),
            node.span,
        );
        node.visit_children_with(self);
    }
}
