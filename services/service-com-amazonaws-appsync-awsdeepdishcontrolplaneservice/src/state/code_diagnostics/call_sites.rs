// SPDX-License-Identifier: CC0-1.0

//! Where each CALL in the read code sits.
//!
//! A line a run logs names the position of the call that wrote it
//! (`crate::ops::evaluate_code::appsync_js`), and what the ENGINE names a call's frame by is
//! the callee's own name — the property for a call made on a member, the callee itself
//! otherwise — rather than where the whole call starts. This pass reads both positions off
//! the syntax tree the compile built, so the engine's frame is taken back to the position the
//! call starts at without reading the code's text for it.
//!
//! One pass over the module, at the same depth and with the same handling of a deeply nested
//! definition as the rule set's own walk (`super::appsync_js_rules`): each level it recurses
//! at is taken onto the HEAP when the thread's own stack runs low, so code that nests as
//! deeply as the modelled maximum length of `code` allows is walked to the end.

use ::swc_common::Spanned;
use ::swc_ecma_ast::{CallExpr, Callee, Expr, MemberProp, Module, Pat, Stmt};
use ::swc_ecma_visit::{Visit, VisitWith};

use crate::ops::evaluate_code::appsync_js::CallSite;

/// How little of the walk's own stack may be left before the next level of it is taken on the
/// HEAP instead, and how much is taken at a time ([`WALK_STACK_GROWTH`]) — the rule set's own
/// handling of a nested definition, for the same reason.
const WALK_RED_ZONE: usize = 128 * 1024;
/// See [`WALK_RED_ZONE`].
const WALK_STACK_GROWTH: usize = 2 * 1024 * 1024;

/// Where every call in `module` sits, in the order the calls sit in the code.
pub(super) fn call_sites(code: &str, module: &Module) -> Vec<CallSite> {
    let mut walked = Calls {
        code,
        sites: Vec::new(),
    };
    module.visit_with(&mut walked);
    walked.sites
}

/// One pass over one module.
struct Calls<'a> {
    /// The code the module was built from, which every position is counted in.
    code: &'a str,
    /// The calls the pass has found so far.
    sites: Vec<CallSite>,
}

impl Visit for Calls<'_> {
    // The nodes a definition nests THROUGH, as the rule set's walk names them.
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

    fn visit_call_expr(&mut self, node: &CallExpr) {
        let stretch = super::offset(node.span.lo)..super::offset(node.span.hi);
        let (line, column) = super::position(self.code, stretch.start);
        self.sites.push(CallSite {
            names_at: super::offset(names_at(node)),
            stretch,
            line,
            column,
        });
        node.visit_children_with(self);
    }
}

/// Where the name the engine names this call's frame by sits: the PROPERTY for a call made on
/// a member (`a.b.c(…)` is named `c`), and the callee itself for every other call.
fn names_at(node: &CallExpr) -> ::swc_common::BytePos {
    let Callee::Expr(callee) = &node.callee else {
        return node.span.lo;
    };
    match &**callee {
        Expr::Member(member) => match &member.prop {
            MemberProp::Ident(name) => name.span.lo,
            MemberProp::PrivateName(name) => name.span().lo,
            MemberProp::Computed(computed) => computed.span.lo,
        },
        other => other.span().lo,
    }
}
