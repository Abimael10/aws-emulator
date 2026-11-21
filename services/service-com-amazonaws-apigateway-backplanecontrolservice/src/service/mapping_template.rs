// SPDX-License-Identifier: CC0-1.0

//! The MAPPING TEMPLATE half of an integration: rendering one against the call it serves.
//!
//! An integration's `requestTemplates` and an integration response's `responseTemplates`
//! are each "a map of Velocity templates that are applied on the request payload"
//! (`Integration$requestTemplates`), keyed by content type. API Gateway RENDERS the
//! template that keys the call and uses the TEXT it produced — as the integration request
//! (which for a `MOCK` is also what the integration answers) and as the response body — so
//! a template is not a body until it has been rendered.
//!
//! # What is rendered, and what is not
//!
//! A template is made of literal text and REFERENCES. A reference names one of the
//! variables API Gateway documents a mapping template against — the method request
//! (`$input`), the stage's own map (`$stageVariables`) and the call's context
//! (`$context`) — optionally calling one of the methods that variable documents. Those
//! are a CLOSED set of values read off this call, so rendering one is a lookup, not an
//! evaluation: nothing here executes anything the template says.
//!
//! The DIRECTIVES a template is performed for are the ones that state something about those
//! same values ([`directives`]): `#set`, because what it states is a BINDING — "the `#set`
//! directive is used to set the value of a reference", so the name it binds is a value of
//! this same closed set, read once and resolved by the references that name it afterwards —
//! and the conditional family (`#if`, `#elseif`, `#else`, `#end`), because what it states is a
//! CHOICE between two pieces of the template decided by a condition over them. Together they
//! are what lets a template name a SUB-DOCUMENT of the payload (the form every API Gateway
//! mapping-template example opens with) and render one document for a request that carries a
//! member and another for one that does not. A directive's own line renders nothing, and the
//! text of a branch this call does not take renders nothing and resolves nothing.
//!
//! The one `#set` that binds no name of the template's own is an OVERRIDE of the request or
//! response the template is building (`$context.requestOverride.header.<name>`,
//! `$context.responseOverride.status`, …): it renders no text, and what it states is carried
//! out of the rendering ([`Overrides`]) for the caller to apply to the request or response it
//! holds.
//!
//! Everything else stays unrendered, and the caller of this module answers such a template
//! the way it answers an integration it cannot perform:
//!
//! * a reference naming a variable, a method or a name outside that set — a `$context`
//!   member this call has no value for, a stage variable the stage does not define, or one
//!   of the `$util` transformations whose exact output AWS's own documentation does not
//!   state (`parseJson`, `validateProperty`), so a rendering of one would be guesswork. The
//!   `$util` conversions whose output a published standard DOES state exactly are performed:
//!   the `application/x-www-form-urlencoded` encode and decode, by the maintained coder for
//!   that format, the base64 encode and decode, by the maintained coder for the alphabet and
//!   padding RFC 4648 states, and the JavaScript string escape, by the rule that language's
//!   own grammar gives for a string literal;
//! * a `$input` reference to the payload where the call has no payload the template can
//!   read (see [`TemplateInputs::payload`]);
//! * a directive that is neither a binding nor a choice (`#foreach`, `#macro`, `#parse`, a
//!   comment), which is a statement whose output this emulator does not decide; the caller
//!   checks for one before rendering.
//!
//! A `$` that opens no reference at all is literal text, exactly as the language treats it,
//! so a template carrying no reference renders to itself.

use std::collections::BTreeMap;

use ::service_common::velocity_template::Expr;

/// Reading the directives a template is performed for: `#set` and the conditional family.
mod directives;
/// The methods a reference may CALL on the value it reached — this plane's policy over the
/// shared reading of the language's value methods.
mod value_methods;

/// What a mapping template's references are rendered against: one call, as the method
/// request and the stage present it.
///
/// Built by whatever is serving the call — the `execute-api` data plane off the wire, and
/// `TestInvokeMethod` out of the members a caller simulates a request with — so a template
/// renders the same way for an invocation that reached a socket and one that never did.
#[derive(Debug, Default, Clone)]
pub struct TemplateInputs {
    /// The payload `$input.body`, `$input.json(…)` and `$input.path(…)` read, or `None`
    /// when this call has none they can read.
    ///
    /// `None` is not an empty payload: it says the payload a template would read is not
    /// known here (the response template of an integration whose own request template
    /// could not be rendered, say), so a reference to it resolves to nothing rather than
    /// to the empty string.
    pub payload: Option<Vec<u8>>,
    /// The path parameters the matched resource path binds, by name.
    pub path: BTreeMap<String, String>,
    /// The query-string parameters the request carries, by name — the first value under
    /// each name, which is the one a single-valued reference names.
    pub querystring: BTreeMap<String, String>,
    /// The headers the request carries, by name as sent; looked up case-insensitively,
    /// because a header name is.
    pub header: BTreeMap<String, String>,
    /// The stage's own `variables`, which `$stageVariables` names.
    pub stage_variables: BTreeMap<String, String>,
    /// The `$context` values of this call, keyed by the member path a template writes
    /// (`httpMethod`, `identity.sourceIp`, …).
    pub context: BTreeMap<String, String>,
}

/// A template this emulator does not render, and why in one phrase.
#[derive(Debug)]
pub struct Unrendered {
    /// What about the template is not rendered — the reference nothing resolves, or the
    /// bound the rendering would pass — for the reason the caller logs and reports.
    pub reason: String,
}

/// What rendering one template produced: the TEXT, and what the template's own override
/// directives state about the request or the response it is building.
#[derive(Debug, Default)]
pub struct Rendering {
    /// The text the template rendered to, which is the body its caller sends.
    pub text: String,
    /// What `$context.requestOverride.…` stated about the INTEGRATION REQUEST.
    pub request: Overrides,
    /// What `$context.responseOverride.…` stated about the METHOD RESPONSE.
    pub response: Overrides,
}

/// The parameters of one side of the call a template OVERRODE, by the location each travels
/// in.
///
/// A mapping template may state a parameter of the request or the response it is building
/// rather than only read the call: `$context.requestOverride.header.<name>`,
/// `.querystring.<name>` and `.path.<name>` name the integration request's parameters, and
/// `$context.responseOverride.header.<name>` and `.status` the method response's. What is
/// collected here is WHAT the template stated; applying it is the caller's, because only the
/// caller holds the request or the response it applies to.
///
/// Each name carries the FIRST value stated for it: a template that overrides one parameter
/// twice keeps the value it stated first, and the later statement is not applied.
#[derive(Debug, Default, Clone)]
pub struct Overrides {
    /// The headers overridden, by name as the template spelled it.
    pub header: BTreeMap<String, String>,
    /// The query-string parameters overridden, by name.
    pub querystring: BTreeMap<String, String>,
    /// The path parameters overridden, by name.
    pub path: BTreeMap<String, String>,
    /// The status the response is to be sent with, as the template stated it.
    ///
    /// A status is a NUMBER, where a parameter carries text: an override assigned a value of
    /// any other type states no status, so this stays `None` and the response is sent with
    /// the status its integration response declares.
    pub status: Option<String>,
}

impl Overrides {
    /// State `value` for `target`, unless this side already carries one for it: the FIRST
    /// statement about a parameter is the one kept.
    fn state(&mut self, target: &directives::OverrideTarget, value: String) {
        use directives::OverrideTarget;

        match target {
            OverrideTarget::Header(name) => {
                self.header.entry(name.clone()).or_insert(value);
            }
            OverrideTarget::QueryString(name) => {
                self.querystring.entry(name.clone()).or_insert(value);
            }
            OverrideTarget::Path(name) => {
                self.path.entry(name.clone()).or_insert(value);
            }
            OverrideTarget::Status => {
                self.status.get_or_insert(value);
            }
        }
    }
}

/// Emulator resource guard, NOT an AWS quota: the largest body one template renders to.
///
/// A template is at most [`crate::ops::put_integration::MAX_TEMPLATE_CHARS`] and each
/// reference in it stands for a value of the call, so the rendering is bounded by the
/// PRODUCT of how many references a developer stored and how large the payload one of them
/// names is — which is what this caps, rather than either factor alone. A small multiple of
/// the template cap: generous enough that no body a backend would answer is refused for its
/// size, and close enough to it that what the stage's response cache may come to hold stays
/// the same order as the configuration it was rendered from. Counted in BYTES, so the check
/// is the length of the text built so far rather than a walk over it.
const MAX_RENDERED_BYTES: usize = 8 * crate::ops::put_integration::MAX_TEMPLATE_CHARS;

/// Emulator resource guard, NOT an AWS quota: the work one rendering may do, counted in the
/// bytes it READS.
///
/// The one piece of work a reference can make large is a query of the PAYLOAD
/// (`$input.json`, `$input.path`): the payload is read as a JSON document once per
/// rendering ([`Render::document`]) and each query looks through it, so without a budget the
/// cost would be the product of two caller-chosen factors — how many such references the
/// stored template carries and how large the payload the request sent is. Charging each
/// query the payload's own size against one budget makes the whole rendering linear in what
/// the request carried, whatever the template asks for.
const MAX_RENDER_WORK: usize = 4 * 1024 * 1024;


/// `template` rendered against `inputs`, refusing outright a template that carries a
/// CONTROL-FLOW directive — the half of the language that is statements rather than values
/// (see the module docs).
///
/// The ONE entry point for a caller that renders a mapping template and reports both
/// refusals the same way. The REST data plane in this crate checks the two halves
/// separately, because it words them differently per integration type; another plane that
/// renders the same language — the WEBSOCKET data plane of API Gateway v2, whose `MOCK`
/// integration's request and response templates are the same `TemplateMap` values — wants
/// the whole rule in one call, and gets it here rather than keeping a second renderer.
pub fn rendered_without_directives(
    template: &str,
    inputs: &TemplateInputs,
) -> Result<String, Unrendered> {
    if super::invoke::carries_unperformed_directive(template) {
        return Err(Unrendered {
            reason: "it carries a Velocity directive, which is a statement this emulator \
                     does not execute"
                .to_string(),
        });
    }
    rendered(template, inputs).map(|rendering| rendering.text)
}

/// `template` rendered against `inputs`, or the first reason this emulator does not render
/// it.
///
/// The work is one pass over the template: every character is either copied, part of a
/// reference or part of a `#set`, and resolving a reference is a map lookup or one query of
/// the payload — which is read as a document once per rendering. So rendering costs a
/// constant per template character, plus one value per reference, plus the reads
/// [`MAX_RENDER_WORK`] bounds; the body it produces is bounded by [`MAX_RENDERED_BYTES`].
pub fn rendered(template: &str, inputs: &TemplateInputs) -> Result<Rendering, Unrendered> {
    let glyphs: Vec<char> = template.chars().collect();
    let mut render = Render {
        inputs,
        document: None,
        bindings: BTreeMap::new(),
        work: MAX_RENDER_WORK,
        request: Overrides::default(),
        response: Overrides::default(),
    };
    let mut rendered = String::with_capacity(template.len());
    // The conditional directives this template has opened and not closed yet: what decides
    // whether the text being read is rendered at all.
    let mut branches: Vec<Branch> = Vec::new();
    let mut at = 0usize;
    while at < glyphs.len() {
        // A directive is a statement about the call — a binding, or a choice between two
        // pieces of the template — and renders nothing itself; a `#` that opens none is text,
        // which is what the language does with one (a colour, a fragment, a comment the
        // caller already refused).
        if glyphs[at] == '#' {
            match directives::directive_at(&glyphs, at) {
                Some(Ok((directive, next))) => {
                    render.perform(&directive, &mut branches)?;
                    at = directives::past_gobbled_newline(&glyphs, next);
                }
                Some(Err(reason)) => {
                    return Err(Unrendered {
                        reason: format!("it carries a directive and {reason}"),
                    })
                }
                None => {
                    if emitting(&branches) {
                        rendered.push('#');
                    }
                    at += 1;
                }
            }
            continue;
        }
        if glyphs[at] != '$' {
            if emitting(&branches) {
                rendered.push(glyphs[at]);
            }
            at += 1;
            continue;
        }
        // A `$` that does not open a reference is text: the language renders it as it
        // stands, which is what a price or a shell-looking literal in a body is.
        if !opens_reference(&glyphs, at) {
            if emitting(&branches) {
                rendered.push('$');
            }
            at += 1;
            continue;
        }
        // The text of a branch this call does NOT take is not rendered at all, so a
        // reference inside it names nothing and resolves nothing: it is stepped over. That
        // is what makes a template whose other branch reads a member this call has no value
        // for renderable — the branch it took is the one that had to resolve.
        if !emitting(&branches) {
            at = reference_at(&glyphs, at, 0).map_or(at + 1, |(_, next)| next);
            continue;
        }
        // From here it IS a reference, so a text this module cannot take apart leaves the
        // template unrendered rather than being copied through as if it were literal: a
        // half-read reference would put the template's own source in the body.
        let (reference, next) = reference_at(&glyphs, at, 0).ok_or_else(|| Unrendered {
            reason: format!(
                "the reference {} is not one this emulator reads",
                spelled(&glyphs, at)
            ),
        })?;
        rendered.push_str(&render.resolved(&reference)?);
        // The length of what has been built, not a walk over it: the check is O(1), so the
        // cap cannot itself become the quadratic cost it exists to prevent. One value may
        // carry the total past the cap, which is why it is checked after each one.
        if rendered.len() > MAX_RENDERED_BYTES {
            return Err(Unrendered {
                reason: format!(
                    "it renders to more than the {MAX_RENDERED_BYTES} bytes this emulator \
                     renders"
                ),
            });
        }
        at = next;
    }
    if !branches.is_empty() {
        return Err(Unrendered {
            reason: "it opens a #if it does not close".to_string(),
        });
    }
    Ok(Rendering {
        text: rendered,
        request: render.request,
        response: render.response,
    })
}

/// How many conditional directives one template may have open at once.
///
/// The template is caller-stored data, so the nesting this emulator follows is bounded here
/// rather than by that text; it sits far above the one or two levels a mapping template is
/// written with.
const MAX_BRANCH_DEPTH: usize = 16;

/// How deeply the expression of a directive may nest before this emulator stops evaluating
/// it.
///
/// The evaluation recurses over the expression tree the language's reader built, and that
/// tree is as deep as the stored template spells it — so the recursion is charged and refused
/// at a bound of its own, the sibling of the one a reference's arguments are read under
/// ([`MAX_REFERENCE_DEPTH`]). The reader bounds the tree it builds too
/// (`::service_common::velocity_template::MAX_NESTING`); this is the bound of the walk over
/// it, so neither rests on the other.
const MAX_EXPRESSION_DEPTH: usize = 16;

/// What an OVERRIDE's value is named as in the reason a rendering that could not write it
/// reports: the value a parameter is overridden with is an expression rather than a reference
/// of the template's own text, so there is no spelling of it to quote.
const OVERRIDE_SOURCE: &str = "the value an override states";

/// One conditional directive of a template that is open: whether the piece being read now is
/// rendered, whether a branch of it has already been taken, and whether the directive itself
/// sits in rendered text.
#[derive(Debug)]
struct Branch {
    /// Whether the text up to the next `#elseif`/`#else`/`#end` is rendered.
    emitting: bool,
    /// Whether a branch of this directive has already been taken, so a later one is not.
    taken: bool,
    /// Whether the `#if` itself was reached in rendered text: a conditional nested inside a
    /// branch nothing takes renders nothing whatever its own condition says.
    inside: bool,
}

/// Whether the text being read now is rendered: every conditional directive open over it has
/// to be rendering its current branch.
fn emitting(branches: &[Branch]) -> bool {
    branches.iter().all(|branch| branch.emitting)
}

/// One rendering in progress: the values it resolves against, the payload read as a
/// document at most ONCE, the names this template's own `#set` directives have bound, and
/// what is left of the reads [`MAX_RENDER_WORK`] allows.
struct Render<'i> {
    inputs: &'i TemplateInputs,
    /// The payload as a JSON document: `None` until a reference needs it, `Some(None)` once
    /// it has been read and is not a JSON document (or there is no payload to read), so
    /// neither the parse nor the refusal happens twice.
    document: Option<Option<::serde_json::Value>>,
    /// What the `#set` directives seen so far have bound, by name.
    bindings: BTreeMap<String, ::serde_json::Value>,
    /// How many more bytes this rendering may READ.
    work: usize,
    /// What the override directives seen so far have stated about the INTEGRATION REQUEST.
    request: Overrides,
    /// What they have stated about the METHOD RESPONSE.
    response: Overrides,
}

impl Render<'_> {
    /// Perform one directive: a `#set` binds a name for the references that follow it, and a
    /// conditional opens, moves on or closes the choice it states.
    ///
    /// Nothing is bound and no condition is evaluated inside a branch this call does not
    /// take: the piece of template it sits in renders nothing, so what it says about the call
    /// is not asked at all.
    fn perform(
        &mut self,
        directive: &directives::Directive,
        branches: &mut Vec<Branch>,
    ) -> Result<(), Unrendered> {
        use directives::Directive;

        let inside = emitting(branches);
        match directive {
            Directive::Set { name, value } => {
                if inside {
                    // A binding may hold a sub-document of the payload, so it is charged the
                    // size of what it came to as well as the read that produced it: the
                    // bindings of one rendering are bounded by the same budget every other
                    // read is charged against, so one cannot be grown by copying it.
                    let bound = self.value_of(value, 0)?;
                    self.spend(weight(&bound))?;
                    self.bindings.insert(name.clone(), bound);
                }
            }
            // An override states a parameter of the request or the response the template is
            // building, so what is kept is the TEXT the expression comes to — the same text a
            // reference to that expression would render ([`Render::written`]), since a
            // parameter carries text and nothing else. The one member that is NOT a parameter
            // is the response's status, which is a number ([`Overrides::status`]).
            Directive::Override(overriding) => {
                if inside {
                    let stated = self.value_of(&overriding.value, 0)?;
                    // The STATUS a response is sent with is a NUMBER, where every other
                    // parameter an override names carries text: a value of any other type
                    // states no status at all — a quoted literal that happens to spell a
                    // status number included — so the response is sent with the status its
                    // integration response declares, and the directive states nothing.
                    if overriding.target == directives::OverrideTarget::Status
                        && !stated.is_number()
                    {
                        return Ok(());
                    }
                    let text = Self::written(stated, OVERRIDE_SOURCE)?;
                    self.spend(text.len())?;
                    let side = if overriding.response {
                        &mut self.response
                    } else {
                        &mut self.request
                    };
                    side.state(&overriding.target, text);
                }
            }
            Directive::If(condition) => {
                if branches.len() >= MAX_BRANCH_DEPTH {
                    return Err(Unrendered {
                        reason: format!(
                            "it nests conditional directives deeper than the \
                             {MAX_BRANCH_DEPTH} this emulator performs"
                        ),
                    });
                }
                let taken = inside && self.truth(condition, 0)?;
                branches.push(Branch {
                    emitting: taken,
                    taken,
                    inside,
                });
            }
            Directive::ElseIf(condition) => {
                let Some(open) = branches.last() else {
                    return Err(Unrendered {
                        reason: "it carries a #elseif that closes no #if".to_string(),
                    });
                };
                let (already, inside) = (open.taken, open.inside);
                let take = !already && inside && self.truth(condition, 0)?;
                if let Some(open) = branches.last_mut() {
                    open.emitting = take;
                    open.taken = already || take;
                }
            }
            Directive::Else => {
                let Some(open) = branches.last_mut() else {
                    return Err(Unrendered {
                        reason: "it carries a #else that closes no #if".to_string(),
                    });
                };
                open.emitting = open.inside && !open.taken;
                open.taken = true;
            }
            Directive::End => {
                if branches.pop().is_none() {
                    return Err(Unrendered {
                        reason: "it carries a #end that closes no #if".to_string(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Whether `condition` holds for this call.
    ///
    /// The truth of a value is the LANGUAGE's rule, so it is the tree's one reading of it
    /// (`::service_common::velocity_template::truthy`): a boolean decides by its own value and
    /// every other value by being there at all, an empty string and a zero among them.
    ///
    /// `depth` is how many expressions this evaluation is already inside
    /// ([`MAX_EXPRESSION_DEPTH`]): an expression nests as deeply as the stored template
    /// spells it, so the recursion over it is charged and refused rather than run as deep as
    /// that text.
    fn truth(&mut self, condition: &Expr, depth: usize) -> Result<bool, Unrendered> {
        let value = self.value_of(condition, depth)?;
        Ok(::service_common::velocity_template::truthy(Some(&value)))
    }

    /// The value one expression of a directive names.
    ///
    /// The operators are the language's own, and what each of them comes to is the shared
    /// reading of its value model; what this plane does not perform is arithmetic over the
    /// call's values and the list and map an expression may build, which name no value of a
    /// method request.
    fn value_of(
        &mut self,
        expression: &Expr,
        depth: usize,
    ) -> Result<::serde_json::Value, Unrendered> {
        use ::serde_json::Value;
        use ::service_common::velocity_template::{equal, ordering, truthy, Operator};

        if depth > MAX_EXPRESSION_DEPTH {
            return Err(Unrendered {
                reason: format!(
                    "it carries an expression nested deeper than the {MAX_EXPRESSION_DEPTH} \
                     this emulator evaluates"
                ),
            });
        }
        let unperformed = || Unrendered {
            reason: "it carries an expression this emulator does not evaluate over the values \
                     of a call"
                .to_string(),
        };
        match expression {
            Expr::Literal(value) => Ok(value.clone()),
            // "double quotes […] will render references": the interpolated form is rendered
            // as a template of its own, so its value is the text it renders to — charged
            // against the same budget as every other read.
            Expr::Interpolated(nodes) => {
                let text = self.interpolated(nodes)?;
                self.spend(text.len())?;
                Ok(Value::String(text))
            }
            Expr::Reference(reference) => {
                self.spend(self.query_cost())?;
                let reference = named(reference).ok_or_else(unperformed)?;
                self.evaluated(&reference)
            }
            Expr::Not(inner) => Ok(Value::Bool(!self.truth(inner, depth + 1)?)),
            Expr::Binary(operator, left, right) => match operator {
                Operator::And => Ok(Value::Bool(
                    self.truth(left, depth + 1)? && self.truth(right, depth + 1)?,
                )),
                Operator::Or => Ok(Value::Bool(
                    self.truth(left, depth + 1)? || self.truth(right, depth + 1)?,
                )),
                Operator::Equal | Operator::NotEqual => {
                    let left = self.value_of(left, depth + 1)?;
                    let right = self.value_of(right, depth + 1)?;
                    let same = equal(Some(&left), Some(&right));
                    Ok(Value::Bool(if *operator == Operator::Equal {
                        same
                    } else {
                        !same
                    }))
                }
                Operator::Less
                | Operator::LessOrEqual
                | Operator::Greater
                | Operator::GreaterOrEqual => {
                    let left = self.value_of(left, depth + 1)?;
                    let right = self.value_of(right, depth + 1)?;
                    // Two values the language cannot order decide nothing, so the comparison
                    // is false rather than the template being refused: that is what a
                    // condition over a member the call does not carry asks.
                    let Ok(order) = ordering(Some(&left), Some(&right)) else {
                        return Ok(Value::Bool(false));
                    };
                    use ::std::cmp::Ordering;
                    Ok(Value::Bool(match operator {
                        Operator::Less => order == Ordering::Less,
                        Operator::LessOrEqual => order != Ordering::Greater,
                        Operator::Greater => order == Ordering::Greater,
                        _ => order != Ordering::Less,
                    }))
                }
                _ => Err(unperformed()),
            },
            // A value this plane builds nothing from: the arithmetic of the language, and the
            // list and map an expression may carry.
            Expr::Negate(_) | Expr::List(_) | Expr::Map(_) => {
                let _ = truthy(None);
                Err(unperformed())
            }
        }
    }

    /// The text an interpolated literal renders to: the references inside it resolved
    /// against this same call, and everything else as written.
    ///
    /// The literal was parsed as a template of its own, so what it carries are the same
    /// nodes; a directive inside one is not performed — it is a statement about the call in a
    /// place that states a value.
    fn interpolated(
        &mut self,
        nodes: &[::service_common::velocity_template::Node],
    ) -> Result<String, Unrendered> {
        use ::service_common::velocity_template::Node;

        let mut rendered = String::new();
        for node in nodes {
            match node {
                Node::Text(text) => rendered.push_str(text),
                Node::Write { reference, .. } => {
                    let reference = named(reference).ok_or_else(|| Unrendered {
                        reason: "it carries a reference this emulator does not read".to_string(),
                    })?;
                    rendered.push_str(&self.resolved(&reference)?);
                }
                _ => {
                    return Err(Unrendered {
                        reason: "it carries a directive inside a value, which this emulator \
                                 does not perform"
                            .to_string(),
                    })
                }
            }
            if rendered.len() > MAX_RENDERED_BYTES {
                return Err(Unrendered {
                    reason: format!(
                        "it renders to more than the {MAX_RENDERED_BYTES} bytes this emulator \
                         renders"
                    ),
                });
            }
        }
        Ok(rendered)
    }

    /// Charge `cost` bytes of reading to this rendering, or refuse it for having done all
    /// the work one request may cause.
    fn spend(&mut self, cost: usize) -> Result<(), Unrendered> {
        match self.work.checked_sub(cost) {
            Some(left) => {
                self.work = left;
                Ok(())
            }
            None => Err(Unrendered {
                reason: format!(
                    "rendering it reads more than the {MAX_RENDER_WORK} bytes one request \
                     may cause this emulator to read"
                ),
            }),
        }
    }

    /// The payload as the JSON document a `$input.json`/`$input.path` query is applied to,
    /// read on the first reference that needs it and kept for the rest of this rendering;
    /// `None` when there is no payload to read or it is not a JSON document.
    fn document(&mut self) -> Result<Option<&::serde_json::Value>, Unrendered> {
        if self.document.is_none() {
            let read = match self.inputs.payload.as_deref() {
                Some(payload) => {
                    // The parse looks through the whole payload, so it is charged for it
                    // once — and the queries below are charged again per query, which is
                    // what bounds the product this module could otherwise cost.
                    self.spend(payload.len())?;
                    ::serde_json::from_slice::<::serde_json::Value>(payload).ok()
                }
                None => None,
            };
            self.document = Some(read);
        }
        Ok(self.document.as_ref().and_then(Option::as_ref))
    }

    /// The size one query of the payload is charged: the payload's own length, which is
    /// what a query may look through.
    fn query_cost(&self) -> usize {
        self.inputs.payload.as_deref().map_or(0, <[u8]>::len)
    }
}

/// How deeply a reference's method arguments may nest before this module stops reading it.
///
/// An argument of a documented method is a quoted literal or one other reference
/// (`$util.urlEncode($input.params('x'))`), so two levels is already past every documented
/// form. The bound is what keeps the reading of a stored template — caller-sized data —
/// from recursing as deeply as that text nests.
const MAX_REFERENCE_DEPTH: usize = 8;

/// How much of a reference is quoted in the reason when nothing resolves it, so one
/// unreadable reference cannot copy a whole template into a log.
const SPELLED_GLYPHS: usize = 64;

/// Whether the `$` at `at` opens a REFERENCE: `$`, optionally `!`, optionally `{`, then a
/// name the language admits.
fn opens_reference(glyphs: &[char], at: usize) -> bool {
    let mut index = at + 1;
    if glyphs.get(index) == Some(&'!') {
        index += 1;
    }
    if glyphs.get(index) == Some(&'{') {
        index += 1;
    }
    glyphs
        .get(index)
        .is_some_and(|first| first.is_ascii_alphabetic() || *first == '_')
}

/// The reference at `at` as the template spells it, clipped to [`SPELLED_GLYPHS`].
fn spelled(glyphs: &[char], at: usize) -> String {
    glyphs
        .iter()
        .skip(at)
        .take(SPELLED_GLYPHS)
        .collect::<String>()
}

/// One reference of a template: the variable it names, the members and methods it reaches
/// through it, and the text the template wrote it as.
#[derive(Debug)]
struct Reference {
    /// The variable the reference opens with (`input`, `context`, `stageVariables`, …).
    variable: String,
    /// What it reaches through that variable, in order.
    steps: Vec<Step>,
    /// The reference exactly as the template wrote it, so a reference nothing resolves is
    /// reported as the developer spelled it.
    source: String,
}

/// One step of a reference: a member of what came before it, or a method called on it.
#[derive(Debug)]
enum Step {
    /// `.name`
    Member(String),
    /// `.name(argument, …)`
    Call(String, Vec<Argument>),
}

/// One argument of a called method: a quoted literal, or another reference.
#[derive(Debug)]
enum Argument {
    Literal(String),
    Reference(Reference),
}

/// The reference the template writes at `at` (the index of its `$`) and the index just
/// past it, or `None` when the text there is not a reference this module reads — which its
/// caller answers as a template it does not render.
///
/// Both spellings of the language are read: the bare `$name.member` and the braced
/// `${name.member}`, each optionally QUIET (`$!name`) — a quiet reference renders the empty
/// string where a plain one renders its own text, and since this module resolves a value or
/// refuses the whole template, the two forms resolve identically here.
///
/// `depth` is how many method arguments this reading is already inside
/// ([`MAX_REFERENCE_DEPTH`]).
fn reference_at(glyphs: &[char], at: usize, depth: usize) -> Option<(Reference, usize)> {
    if depth > MAX_REFERENCE_DEPTH {
        return None;
    }
    let mut index = at + 1;
    if glyphs.get(index) == Some(&'!') {
        index += 1;
    }
    let braced = glyphs.get(index) == Some(&'{');
    if braced {
        index += 1;
    }
    let (variable, next) = identifier_at(glyphs, index)?;
    index = next;
    let mut steps = Vec::new();
    loop {
        if glyphs.get(index) == Some(&'.') {
            let Some((name, after_name)) = identifier_at(glyphs, index + 1) else {
                break;
            };
            if glyphs.get(after_name) == Some(&'(') {
                let (arguments, after_call) = arguments_at(glyphs, after_name + 1, depth + 1)?;
                steps.push(Step::Call(name, arguments));
                index = after_call;
            } else {
                steps.push(Step::Member(name));
                index = after_name;
            }
            continue;
        }
        // The BRACKETED spelling of the same member, which is how a key that is not a bare
        // name is reached: `$context.authorizer.claims['cognito:username']`. A Cognito
        // authorizer publishes namespaced claims, whose `:` no identifier admits, so the
        // bracketed form is the only way a template can read one.
        if glyphs.get(index) == Some(&'[') {
            let Some((key, after_key)) = quoted_key_at(glyphs, index + 1) else {
                break;
            };
            if glyphs.get(after_key) != Some(&']') {
                break;
            }
            steps.push(Step::Member(key));
            index = after_key + 1;
            continue;
        }
        break;
    }
    if braced {
        if glyphs.get(index) != Some(&'}') {
            // An unterminated `${…` is a reference this module cannot read.
            return None;
        }
        index += 1;
    }
    let source: String = glyphs.get(at..index)?.iter().collect();
    Some((
        Reference {
            variable,
            steps,
            source,
        },
        index,
    ))
}

/// The `$context` member whose value is a MAP an authorizer returned rather than a fixed set
/// of members: `$context.authorizer`, whose `claims` a user-pool authorizer fills and whose
/// other keys a custom authorizer chooses. A key it does not carry is an absent value, so a
/// reference to one renders as nothing.
const AUTHORIZER_MEMBER: &str = "authorizer";

/// The identifier at `at` and the index just past it: a letter or `_`, then letters,
/// digits, `_` and `-`, which is what the language admits in a name.
fn identifier_at(glyphs: &[char], at: usize) -> Option<(String, usize)> {
    let first = glyphs.get(at)?;
    if !first.is_ascii_alphabetic() && *first != '_' {
        return None;
    }
    let mut index = at;
    let mut name = String::new();
    while let Some(glyph) = glyphs.get(index) {
        if glyph.is_ascii_alphanumeric() || *glyph == '_' || *glyph == '-' {
            name.push(*glyph);
            index += 1;
        } else {
            break;
        }
    }
    Some((name, index))
}

/// The quoted key at `at` and the index just past its closing quote: the subscript of a
/// bracketed member access (`['cognito:username']`, `["cognito:username"]`).
///
/// Either quote, as the language admits both, and the key is taken verbatim — a member name is
/// whatever the value it names is keyed by, which for a token's claim is the claim's own name.
/// `None` for a subscript that is not a quoted string, which leaves the text unread rather
/// than guessed at.
fn quoted_key_at(glyphs: &[char], at: usize) -> Option<(String, usize)> {
    let quote = *glyphs.get(at)?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let mut index = at + 1;
    let mut key = String::new();
    loop {
        let glyph = *glyphs.get(index)?;
        index += 1;
        if glyph == quote {
            break;
        }
        key.push(glyph);
    }
    (!key.is_empty()).then_some((key, index))
}

/// The arguments of a method call opened at `at` (just past its `(`), and the index just
/// past the closing `)`.
///
/// Each argument is a quoted literal or another reference, which are the two forms a
/// mapping template's documented methods take. Anything else leaves the whole text
/// unread, so a template this module cannot take apart is never rendered from a partial
/// reading of it. `depth` bounds how far a nested argument is followed
/// ([`MAX_REFERENCE_DEPTH`]).
fn arguments_at(glyphs: &[char], at: usize, depth: usize) -> Option<(Vec<Argument>, usize)> {
    if depth > MAX_REFERENCE_DEPTH {
        return None;
    }
    let mut index = at;
    let mut arguments = Vec::new();
    loop {
        while glyphs.get(index).is_some_and(|glyph| glyph.is_whitespace()) {
            index += 1;
        }
        match glyphs.get(index)? {
            ')' => return Some((arguments, index + 1)),
            ',' => index += 1,
            '\'' | '"' => {
                let quote = *glyphs.get(index)?;
                index += 1;
                let mut literal = String::new();
                loop {
                    let glyph = *glyphs.get(index)?;
                    index += 1;
                    if glyph == quote {
                        break;
                    }
                    literal.push(glyph);
                }
                arguments.push(Argument::Literal(literal));
            }
            '$' => {
                let (reference, next) = reference_at(glyphs, index, depth + 1)?;
                arguments.push(Argument::Reference(reference));
                index = next;
            }
            _ => return None,
        }
    }
}

impl Render<'_> {
    /// The TEXT `reference` renders to, or the reason this emulator renders none.
    ///
    /// A value renders as the text the language writes it as: a string as itself, a number
    /// or a boolean as it spells, and a value the call does not have as nothing. An OBJECT
    /// or an ARRAY is a Java object whose own rendering AWS does not state, so it is left
    /// unresolved rather than written as something else — which is why a template that
    /// means to read inside one BINDS it first (`#set`) and renders the members it names.
    fn resolved(&mut self, reference: &Reference) -> Result<String, Unrendered> {
        let value = self.evaluated(reference)?;
        Self::written(value, &reference.source)
    }

    /// The TEXT a value renders as, or the reason this emulator renders none for it.
    ///
    /// A value renders as the text the language writes it as: a string as itself, a number or
    /// a boolean as it spells, and a value the call does not have as nothing. An OBJECT or an
    /// ARRAY is a Java object whose own rendering AWS does not state, so it is left unwritten
    /// rather than written as something else — which is why a template that means to read
    /// inside one BINDS it first (`#set`) and renders the members it names. `source` is what
    /// named it, for the reason a caller reports.
    fn written(value: ::serde_json::Value, source: &str) -> Result<String, Unrendered> {
        match value {
            ::serde_json::Value::String(text) => Ok(text),
            node @ (::serde_json::Value::Number(_) | ::serde_json::Value::Bool(_)) => {
                Ok(node.to_string())
            }
            ::serde_json::Value::Null => Ok(String::new()),
            ::serde_json::Value::Object(_) | ::serde_json::Value::Array(_) => Err(Unrendered {
                reason: format!(
                    "it names {source}, which this emulator resolves no value of this call for"
                ),
            }),
        }
    }

    /// The VALUE `reference` names, or the reference itself when this emulator resolves
    /// none.
    ///
    /// A reference may CALL a method on the value it has reached — `$input.path('$.x').length()`,
    /// `$name.toUpperCase()` — because the values a mapping template reads are Java objects and
    /// such a call is a method of the TYPE the selected value has, not of the variable the
    /// reference opened with. So the steps are split: the leading ones name a value of this call
    /// ([`Self::evaluated_steps`]) and the trailing calls this plane answers on a value are
    /// applied to it in order ([`value_methods`]). A reference carrying a call neither the
    /// variable surface nor that policy answers stays unresolved, exactly as before.
    fn evaluated(&mut self, reference: &Reference) -> Result<::serde_json::Value, Unrendered> {
        let (steps, calls) = value_methods::split(&reference.steps);
        let mut value = self.evaluated_steps(&reference.variable, steps, &reference.source)?;
        for (method, arguments) in calls {
            // The argument, where the method takes one: resolved against this same call, as
            // every other method's argument is.
            let argument = match arguments {
                [] => None,
                _ => Some(self.one_argument(arguments)?.ok_or_else(|| Unrendered {
                    reason: format!(
                        "it names {}, whose {method} is called with arguments this emulator \
                         does not read",
                        reference.source
                    ),
                })?),
            };
            value = value_methods::called(&value, method, argument.as_deref()).ok_or_else(|| {
                Unrendered {
                    reason: format!(
                        "it names {}, and {method} is no method this emulator evaluates over \
                         the value it reached",
                        reference.source
                    ),
                }
            })?;
            self.spend(weight(&value))?;
        }
        Ok(value)
    }

    /// The VALUE the variable `variable` and the steps `steps` name, or the reason this
    /// emulator resolves none. `source` is the reference as the template wrote it, for the
    /// reason a caller reports.
    ///
    /// Every variable below is one API Gateway documents a mapping template against, or one
    /// this template's own `#set` bound; every method is one that variable documents. A
    /// reference outside that set is answered as unresolved rather than rendered as
    /// something close to it.
    fn evaluated_steps(
        &mut self,
        variable: &str,
        steps: &[Step],
        source: &str,
    ) -> Result<::serde_json::Value, Unrendered> {
        let text = ::serde_json::Value::String;
        let unresolved = || Unrendered {
            reason: format!(
                "it names {source}, which this emulator resolves no value of this call for"
            ),
        };
        match variable {
            // "$input" — "represents the method request payload and parameters to be
            // processed by the mapping template".
            "input" => match steps {
                // `$input.body` — "returns the raw request payload as a string".
                [Step::Member(member)] if member == "body" => {
                    let bytes = self
                        .inputs
                        .payload
                        .as_deref()
                        .map(<[u8]>::len)
                        .ok_or_else(unresolved)?;
                    self.spend(bytes)?;
                    let payload = self.inputs.payload.as_deref().unwrap_or_default();
                    // As TEXT, which is what the reference stands for: a payload that is
                    // not text is left unresolved rather than written out with its
                    // undecodable bytes replaced, which would put a body in the response
                    // that the caller never sent.
                    ::std::str::from_utf8(payload)
                        .map(|payload| text(payload.to_string()))
                        .map_err(|_| Unrendered {
                            reason: format!(
                                "it names {source}, and this call's payload is not text"
                            ),
                        })
                }
                // `$input.json(x)` — "evaluates a JSONPath expression and returns the
                // results as a JSON string": the selected node written as JSON.
                [Step::Call(method, arguments)] if method == "json" => {
                    let path = self.one_argument(arguments)?.ok_or_else(unresolved)?;
                    match self.queried(&path)?.ok_or_else(unresolved)? {
                        Some(node) => Ok(text(node.to_string())),
                        // A path that selects nothing writes nothing.
                        None => Ok(text(String::new())),
                    }
                }
                // `$input.path(x)` — "takes a JSONPath expression string and returns a
                // JSON object representation of the result": the selected node itself,
                // which a `#set` binds whole and a reference renders when it is a value
                // the language writes as text ([`Render::resolved`]).
                [Step::Call(method, arguments)] if method == "path" => {
                    let path = self.one_argument(arguments)?.ok_or_else(unresolved)?;
                    Ok(self
                        .queried(&path)?
                        .ok_or_else(unresolved)?
                        .unwrap_or(::serde_json::Value::Null))
                }
                // `$input.params(x)` — "returns the method request parameter value from the
                // path, query string, or header (in that order) given a parameter name".
                [Step::Call(method, arguments)] if method == "params" && !arguments.is_empty() => {
                    let name = self.one_argument(arguments)?.ok_or_else(unresolved)?;
                    Ok(text(parameter(self.inputs, &name).unwrap_or_default()))
                }
                // `$input.params()` — "returns a map of all request parameters": the
                // parameters of THIS call keyed by where each came from, which is the value
                // every documented use of this form reads through — a source and a name
                // (`$input.params().path.get('id')`), or the whole map bound by a `#set` and
                // read the same way. So the steps after it are the keys of that map, read
                // the way a reference reads the keys of every other map of the call.
                [Step::Call(method, arguments), read @ ..]
                    if method == "params" && arguments.is_empty() =>
                {
                    // The keys are read first: an argument of a `get` is itself resolved
                    // against this call, which the lookup below holds no borrow for.
                    let mut keys = Vec::with_capacity(read.len());
                    for step in read {
                        match step {
                            Step::Member(name) => keys.push(name.clone()),
                            Step::Call(method, arguments) if method == "get" => {
                                keys.push(self.one_argument(arguments)?.ok_or_else(unresolved)?);
                            }
                            Step::Call(_, _) => return Err(unresolved()),
                        }
                    }
                    let value = parameters_at(self.inputs, &keys).ok_or_else(unresolved)?;
                    self.spend(weight(&value))?;
                    Ok(value)
                }
                _ => Err(unresolved()),
            },
            // "$stageVariables" — the stage's own map; `.<name>` and `.get('<name>')` name
            // the same entry. A name the stage defines no value for resolves to nothing at
            // all, which is how the rest of this data plane reads a reference to one.
            "stageVariables" => {
                let name = match steps {
                    [Step::Member(name)] => name.clone(),
                    [Step::Call(method, arguments)] if method == "get" => {
                        self.one_argument(arguments)?.ok_or_else(unresolved)?
                    }
                    _ => return Err(unresolved()),
                };
                self.inputs
                    .stage_variables
                    .get(&name)
                    .cloned()
                    .map(text)
                    .ok_or_else(unresolved)
            }
            // "$context" — "contains the contextual information of your request". Only the
            // members this call has a value for resolve, with the one exception the
            // `authorizer` map below states.
            "context" => {
                let mut named = Vec::with_capacity(steps.len());
                for step in steps {
                    match step {
                        Step::Member(name) => named.push(name.clone()),
                        // `get('name')` is the method a MAP of the context is read by, which
                        // is the other spelling of the bracketed member access above — the
                        // same member, named by its key.
                        Step::Call(method, arguments) if method == "get" => {
                            match self.one_argument(arguments)? {
                                Some(key) => named.push(key),
                                None => return Err(unresolved()),
                            }
                        }
                        Step::Call(_, _) => return Err(unresolved()),
                    }
                }
                self.inputs
                    .context
                    .get(&named.join("."))
                    .cloned()
                    .or_else(|| {
                        // `$context.authorizer` is not a fixed set of members: it is the MAP
                        // an authorizer returned as the context of its decision — its
                        // `claims` for a user-pool authorizer, whatever a custom one put
                        // there — so a key it does not carry is an ABSENT VALUE rather than a
                        // reference naming something outside the language. It renders as
                        // nothing, the way a lookup in every other map of the call does
                        // (`$input.params` of a parameter the request did not send).
                        named
                            .first()
                            .is_some_and(|first| first == AUTHORIZER_MEMBER)
                            .then(String::new)
                    })
                    .map(text)
                    .ok_or_else(unresolved)
            }
            // "$util" — "contains utility functions", each documented with the conversion
            // it performs. The conversions whose output a published standard STATES exactly
            // are performed — the two form-encoding conversions and the two base64
            // conversions by the maintained coders for those formats, and the JavaScript
            // string escape by the rule that language's own grammar gives
            // ([`escaped_as_javascript`]); every other `$util` method (`parseJson`,
            // `validateProperty`) stays unresolved, for the reason the module docs give.
            "util" => match steps {
                [Step::Call(method, arguments)] => {
                    let argument = self.one_argument(arguments)?.ok_or_else(unresolved)?;
                    self.spend(argument.len())?;
                    match method.as_str() {
                        // "$util.urlEncode(String) — Converts a string into
                        // 'application/x-www-form-urlencoded' format."
                        "urlEncode" => Ok(text(
                            ::form_urlencoded::byte_serialize(argument.as_bytes())
                                .collect::<String>(),
                        )),
                        // "$util.urlDecode(String) — Decodes an
                        // 'application/x-www-form-urlencoded' string back into its
                        // original form." The decoder is the framework's one QUERY decoder,
                        // which resolves every escape and folds `+` to a space as that
                        // format's rule is.
                        "urlDecode" => {
                            Ok(text(::smithy_server_runtime::percent_decode(&argument)))
                        }
                        // "$util.escapeJavaScript() — Escapes the characters in a string
                        // using JavaScript string rules", which is what lets a template put
                        // a caller's own text inside a quoted string of the document it
                        // renders.
                        "escapeJavaScript" => Ok(text(escaped_as_javascript(&argument))),
                        // "$util.base64Encode(data) — Encodes the data into a base64-encoded
                        // string", by the maintained coder for the format, over the standard
                        // alphabet and padding RFC 4648 states — which is the encoding every
                        // AWS api carrying a blob in a JSON document reads.
                        "base64Encode" => Ok(text(::base64::Engine::encode(
                            &::base64::engine::general_purpose::STANDARD,
                            argument.as_bytes(),
                        ))),
                        // "$util.base64Decode(data) — Decodes the data from a base64-encoded
                        // string." Text that is not that encoding, and bytes that are not
                        // the text a template writes into a document, decode to nothing this
                        // module can render, so such a reference stays unresolved rather than
                        // rendering a guess.
                        "base64Decode" => ::base64::Engine::decode(
                            &::base64::engine::general_purpose::STANDARD,
                            argument.as_bytes(),
                        )
                        .ok()
                        .and_then(|bytes| String::from_utf8(bytes).ok())
                        .map(text)
                        .ok_or_else(unresolved),
                        _ => Err(unresolved()),
                    }
                }
                _ => Err(unresolved()),
            },
            // A name this template's own `#set` bound, whose members the references after it
            // read: the value is this call's, read once when it was bound, so what a
            // reference to it names is a lookup like every other one.
            //
            // The lookup COPIES only the node it selects, not the document it selects from,
            // and is charged that much against this rendering's budget: a reference to a
            // member of a bound payload costs the member, so a template carrying many of
            // them costs what it renders rather than the product of the payload's size and
            // how many times it names one.
            //
            // A member the bound value does not carry is an ABSENT VALUE, as it is in every
            // other map of the call ([`AUTHORIZER_MEMBER`]): it renders as nothing, and
            // reading further through it goes on naming nothing.
            bound if self.bindings.contains_key(bound) => {
                // The keys the reference walks are read first: an argument of a call is
                // itself resolved against this call, which the walk below holds no borrow
                // for.
                let mut keys = Vec::with_capacity(steps.len());
                for step in steps {
                    match step {
                        Step::Member(name) => keys.push(name.clone()),
                        Step::Call(method, arguments) if method == "get" => {
                            keys.push(self.one_argument(arguments)?.ok_or_else(unresolved)?);
                        }
                        Step::Call(_, _) => return Err(unresolved()),
                    }
                }
                let mut selected = self.bindings.get(bound);
                for key in &keys {
                    selected = selected
                        .and_then(::serde_json::Value::as_object)
                        .and_then(|members| members.get(key));
                }
                let value = selected.cloned().unwrap_or(::serde_json::Value::Null);
                self.spend(weight(&value))?;
                Ok(value)
            }
            _ => Err(unresolved()),
        }
    }

    /// The single argument a one-argument method was called with, resolved to its text, or
    /// `None` when it was called with no argument or with several.
    fn one_argument(&mut self, arguments: &[Argument]) -> Result<Option<String>, Unrendered> {
        match arguments {
            [Argument::Literal(literal)] => Ok(Some(literal.clone())),
            // An argument that is itself a reference is resolved first, so
            // `$input.params($stageVariables.name)` names the parameter that variable
            // names.
            [Argument::Reference(reference)] => self.resolved(reference).map(Some),
            _ => Ok(None),
        }
    }

    /// The node `path` selects in the payload, or `None` for a payload or a path this
    /// emulator cannot query — and `Some(None)` for a path that selects nothing.
    ///
    /// The path is read and evaluated by `service_common`'s ONE bounded single-value JSON
    /// path reader, which is where this emulator's rule for such a path lives: a path is
    /// accepted only as a REFERENCE path (`$`, `.name`, `['name']`, `[index]`, a bounded
    /// number of them), its segments are what the maintained query engine is handed, and a
    /// path naming more than one place is refused rather than evaluated. That is also what
    /// makes one query's cost the document it looks through — the cost each query is
    /// charged ([`Render::query_cost`]) — because each accepted segment selects at most one
    /// node from at most one node.
    ///
    /// Three answers, told apart: a path that reader does not evaluate leaves the template
    /// unrendered, a path that selects no place writes nothing, and a path that names one
    /// value writes it.
    fn queried(&mut self, path: &str) -> Result<Option<Option<::serde_json::Value>>, Unrendered> {
        let Some(segments) = ::service_common::json_reference_path::segments(path) else {
            return Ok(None);
        };
        self.spend(self.query_cost())?;
        let Some(document) = self.document()? else {
            return Ok(None);
        };
        Ok(Some(
            ::service_common::json_reference_path::select_segments(&segments, document).cloned(),
        ))
    }
}

/// The REFERENCE this plane's own reader names, for one the language's reader parsed.
///
/// The two readers describe the same thing — the variable a reference opens with and what it
/// reaches through it — so a reference read out of a directive is resolved by the same
/// resolution a reference of the template body is ([`Render::evaluated`]). `None` for one
/// whose steps this plane does not resolve: an index that is no name, and an argument that is
/// neither a literal nor another reference, which are the two forms a documented method
/// takes.
fn named(reference: &::service_common::velocity_template::Reference) -> Option<Reference> {
    use ::serde_json::Value;
    use ::service_common::velocity_template::{Access, Expr};

    let mut steps = Vec::with_capacity(reference.accesses.len());
    let mut source = format!("${}", reference.root);
    for access in &reference.accesses {
        match access {
            Access::Prop(name) => {
                source.push('.');
                source.push_str(name);
                steps.push(Step::Member(name.clone()));
            }
            // The bracketed spelling of the same member, which is how a key that is not a
            // bare name is reached (`$context.authorizer.claims['cognito:username']`).
            Access::Index(Expr::Literal(Value::String(key))) => {
                source.push_str(&format!("['{key}']"));
                steps.push(Step::Member(key.clone()));
            }
            Access::Index(_) => return None,
            Access::Call(name, arguments) => {
                source.push('.');
                source.push_str(name);
                source.push_str("(…)");
                let mut taken = Vec::with_capacity(arguments.len());
                for argument in arguments {
                    match argument {
                        Expr::Literal(Value::String(text)) => {
                            taken.push(Argument::Literal(text.clone()));
                        }
                        Expr::Reference(inner) => {
                            taken.push(Argument::Reference(named(inner)?));
                        }
                        _ => return None,
                    }
                }
                steps.push(Step::Call(name.clone(), taken));
            }
        }
    }
    Some(Reference {
        variable: reference.root.clone(),
        steps,
        source,
    })
}

/// How many bytes a value a binding holds is charged.
///
/// The text of it, which is the most a reference to it can render and so the most holding it
/// can cost: a binding is charged that against the same budget every read is charged against
/// ([`MAX_RENDER_WORK`]), so a template cannot grow one binding by copying another.
fn weight(value: &::serde_json::Value) -> usize {
    match value {
        ::serde_json::Value::String(text) => text.len(),
        ::serde_json::Value::Null => 0,
        other => other.to_string().len(),
    }
}

/// The value of the method request parameter `name`: the path parameter, then the query
/// string, then the header, which is the order the method documents. A header is matched
/// case-insensitively, because a header name is.
fn parameter(inputs: &TemplateInputs, name: &str) -> Option<String> {
    if let Some(bound) = inputs.path.get(name) {
        return Some(bound.clone());
    }
    if let Some(sent) = inputs.querystring.get(name) {
        return Some(sent.clone());
    }
    inputs
        .header
        .iter()
        .find(|(sent, _)| sent.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

/// The key the map `$input.params()` names carries the request's HEADERS under: the one
/// source whose names are matched case-insensitively, because a header name is.
const HEADER_SOURCE: &str = "header";

/// The three sources a method request parameter comes from, each under the key the map
/// `$input.params()` names carries it under — the same three [`parameter`] reads, in the
/// order it reads them.
fn parameter_sources(inputs: &TemplateInputs) -> [(&'static str, &BTreeMap<String, String>); 3] {
    [
        ("path", &inputs.path),
        ("querystring", &inputs.querystring),
        (HEADER_SOURCE, &inputs.header),
    ]
}

/// The value the map `$input.params()` names carries at `keys`, or `None` for a reference
/// that reads PAST the shape of that map.
///
/// With no key it is the whole map: one entry per source, each the parameters that source
/// carries by name. With a source it is that source's own map, and with a source and a name
/// it is the value that source carries under that name. A source or a name the map does not
/// carry is an ABSENT value rather than a reference naming something outside the language,
/// the way a key no other map of the call carries is ([`Render::evaluated`]): it renders as
/// nothing, exactly as `$input.params` of a parameter the request did not send does. A key
/// BEYOND a name is neither: a parameter's value is text, which carries no key at all, so
/// such a reference names nothing this call has a value for and leaves the template
/// unrendered rather than rendering the empty string for it.
fn parameters_at(inputs: &TemplateInputs, keys: &[String]) -> Option<::serde_json::Value> {
    use ::serde_json::Value;

    let sources = parameter_sources(inputs);
    let Some((source, rest)) = keys.split_first() else {
        return Some(Value::Object(
            sources
                .iter()
                .map(|(key, entries)| ((*key).to_string(), named_parameters(entries)))
                .collect(),
        ));
    };
    let Some((key, entries)) = sources.iter().find(|(key, _)| key == source) else {
        return Some(Value::Null);
    };
    let Some((name, rest)) = rest.split_first() else {
        return Some(named_parameters(entries));
    };
    // A value of the map is a parameter's own text, which carries no further key.
    if !rest.is_empty() {
        return None;
    }
    let sent = if *key == HEADER_SOURCE {
        entries
            .iter()
            .find(|(sent, _)| sent.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    } else {
        entries.get(name).cloned()
    };
    Some(sent.map_or(Value::Null, Value::String))
}

/// One source's parameters as the map a reference reads the names of.
fn named_parameters(entries: &BTreeMap<String, String>) -> ::serde_json::Value {
    ::serde_json::Value::Object(
        entries
            .iter()
            .map(|(name, value)| (name.clone(), ::serde_json::Value::String(value.clone())))
            .collect(),
    )
}

/// `text` with the characters a JavaScript string literal cannot carry as they stand
/// replaced by their escapes, which is what `$util.escapeJavaScript` performs.
///
/// The rule is ECMAScript's own string-literal grammar, which states exactly which
/// characters need an escape and what each escape is: the quote characters and the escape
/// character itself, the five escapes the grammar names (`\b`, `\t`, `\n`, `\f`, `\r`), and
/// every other control character as the `\uXXXX` the grammar spells it with. The forward
/// slash is escaped too, because a rendering of this is embedded in a document a script may
/// carry and `</` has to stay inert there. Nothing else is touched: a template that escapes
/// text and one that writes it put the same characters on the wire.
fn escaped_as_javascript(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\'' => escaped.push_str("\\'"),
            '\\' => escaped.push_str("\\\\"),
            '/' => escaped.push_str("\\/"),
            '\u{8}' => escaped.push_str("\\b"),
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            '\u{c}' => escaped.push_str("\\f"),
            '\r' => escaped.push_str("\\r"),
            other if other.is_control() => {
                escaped.push_str(&format!("\\u{:04X}", u32::from(other)));
            }
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The TEXT `template` renders to against `inputs`, which is what the cases below assert
    /// over; the overrides a rendering also carries have cases of their own
    /// ([`tests::an_override_states_the_parameter_it_names`]).
    fn rendered(template: &str, inputs: &TemplateInputs) -> Result<String, Unrendered> {
        super::rendered(template, inputs).map(|rendering| rendering.text)
    }

    /// The inputs the cases below render against: one of each kind of value a template can
    /// name.
    fn inputs() -> TemplateInputs {
        TemplateInputs {
            payload: Some(br#"{"id":"a1","qty":2,"nested":{"deep":"value"}}"#.to_vec()),
            path: [("pet".to_string(), "fido".to_string())].into_iter().collect(),
            querystring: [("tenant".to_string(), "acme".to_string())]
                .into_iter()
                .collect(),
            header: [("X-Trace".to_string(), "t-1".to_string())]
                .into_iter()
                .collect(),
            stage_variables: [("backend".to_string(), "beta".to_string())]
                .into_iter()
                .collect(),
            context: [
                ("httpMethod".to_string(), "POST".to_string()),
                ("stage".to_string(), "test".to_string()),
                (
                    "requestId".to_string(),
                    "6c3e2a77-0000-4000-8000-9f1c2d3e4f50".to_string(),
                ),
                (
                    "authorizer.claims.sub".to_string(),
                    "6b1d0f0e-user".to_string(),
                ),
                (
                    "authorizer.claims.cognito:username".to_string(),
                    "alice".to_string(),
                ),
            ]
            .into_iter()
            .collect(),
        }
    }

    #[test]
    fn every_documented_variable_renders_the_value_the_call_carries() {
        let cases: &[(&str, &str)] = &[
            // Literal text, including a `$` that opens no reference.
            ("{\"literal\":true}", "{\"literal\":true}"),
            ("cost is $12", "cost is $12"),
            // The method request.
            (r#"{"tenant":"$input.params('tenant')"}"#, r#"{"tenant":"acme"}"#),
            (r#"$input.params("X-TRACE")"#, "t-1"),
            ("$input.params('pet')", "fido"),
            ("$input.params('absent')", ""),
            ("${input.params('tenant')}", "acme"),
            ("$!input.params('tenant')", "acme"),
            ("$input.body", r#"{"id":"a1","qty":2,"nested":{"deep":"value"}}"#),
            ("$input.json('$.id')", "\"a1\""),
            ("$input.json('$.qty')", "2"),
            ("$input.path('$.id')", "a1"),
            ("$input.path('$.qty')", "2"),
            ("$input.path('$.nested.deep')", "value"),
            ("$input.json('$.missing')", ""),
            // The two form-encoding conversions and the JavaScript string escape, which is
            // what puts a caller's own text inside a quoted string of a rendered document.
            ("$util.urlEncode('a b&c')", "a+b%26c"),
            ("$util.urlDecode('a+b%26c')", "a b&c"),
            (
                r#"{"lastBody":"$util.escapeJavaScript($input.body)"}"#,
                r#"{"lastBody":"{\"id\":\"a1\",\"qty\":2,\"nested\":{\"deep\":\"value\"}}"}"#,
            ),
            // The two base64 conversions, which is what puts a payload in the blob member of
            // the document an integration request renders, and reads one back out.
            ("$util.base64Encode('a b&c')", "YSBiJmM="),
            ("$util.base64Decode('YSBiJmM=')", "a b&c"),
            ("$util.base64Encode('')", ""),
            (
                "$util.base64Decode($util.base64Encode($input.body))",
                r#"{"id":"a1","qty":2,"nested":{"deep":"value"}}"#,
            ),
            // The stage and the context.
            ("$stageVariables.backend", "beta"),
            ("$stageVariables.get('backend')", "beta"),
            ("$context.httpMethod/$context.stage", "POST/test"),
            // The id the call is answered under, which the data plane carries for every
            // request it serves.
            ("$context.requestId", "6c3e2a77-0000-4000-8000-9f1c2d3e4f50"),
            // An authorizer's claims, by the bare member path and by the two spellings a
            // claim whose name is no bare identifier is reached by.
            ("$context.authorizer.claims.sub", "6b1d0f0e-user"),
            ("$context.authorizer.claims['cognito:username']", "alice"),
            ("$context.authorizer.claims[\"cognito:username\"]", "alice"),
            ("$context.authorizer.claims.get('cognito:username')", "alice"),
            ("${context.authorizer.claims['cognito:username']}", "alice"),
            // A key the authorizer's context map does not carry is an absent VALUE, so it
            // renders as nothing — the same way a parameter the request did not send does.
            ("$context.authorizer.claims.email", ""),
            ("$context.authorizer.principalId", ""),
        ];
        let inputs = inputs();
        for (template, expected) in cases {
            let rendered = rendered(template, &inputs);
            assert_eq!(
                rendered.as_deref().ok(),
                Some(*expected),
                "case {template:?}: expected {expected:?}, got {rendered:?}"
            );
        }
    }

    /// RULE: a `#set` binds a name to the value its expression names, the references after it
    /// render the members they reach through that name, and the directive itself renders
    /// nothing.
    #[test]
    fn a_set_binds_a_name_the_references_after_it_render() {
        /// (case, the template, what it renders to)
        type Row<'a> = (&'a str, &'a str, &'a str);
        let rows: &[Row<'_>] = &[
            (
                "the form every mapping-template example opens with: a bound payload, read \
                 member by member",
                r#"#set($root = $input.path('$')) {"id":"$root.id","qty":$root.qty}"#,
                r#" {"id":"a1","qty":2}"#,
            ),
            (
                "a bound SUB-document, which is what a reference cannot render on its own",
                r#"#set($n = $input.path('$.nested')) {"deep":"$n.deep"}"#,
                r#" {"deep":"value"}"#,
            ),
            (
                "the directive on a line of its own, whose line break renders nothing",
                "#set($root = $input.path('$'))\n{\"id\":\"$root.id\"}",
                "{\"id\":\"a1\"}",
            ),
            (
                "the formal spelling, and a member reached by the method a map documents",
                "#{set}($root = $input.path('$'))$root.get('id')",
                "a1",
            ),
            (
                "a second binding, read through the first",
                "#set($root = $input.path('$'))#set($q = $root.qty)$q",
                "2",
            ),
            (
                "a bound literal and a bound number",
                "#set($a = 'alpha')#set($b = 7)$a/$b",
                "alpha/7",
            ),
            (
                "a bound literal the language interpolates",
                r#"#set($a = "pet $input.params('pet')")$a"#,
                "pet fido",
            ),
            (
                "a member the bound value does not carry, which is an absent value",
                "#set($root = $input.path('$'))[$root.absent]",
                "[]",
            ),
            (
                "a member reached through one the bound value does not carry",
                "#set($root = $input.path('$'))[$root.absent.deeper]",
                "[]",
            ),
            (
                "a `#` that opens no directive, which is text",
                "#set($a = 1){\"colour\":\"#fff\",\"a\":$a}",
                r##"{"colour":"#fff","a":1}"##,
            ),
            (
                "the binding of a parameter, which is text either way",
                "#set($p = $input.params('tenant'))$p",
                "acme",
            ),
        ];
        let inputs = inputs();
        for (case, template, expected) in rows {
            let actual = rendered(template, &inputs);
            assert_eq!(
                actual.as_deref().ok(),
                Some(*expected),
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a `#set` this emulator cannot read, and a reference to a name nothing bound,
    /// leave the template unrendered rather than rendering part of it.
    #[test]
    fn a_set_this_emulator_cannot_read_leaves_the_template_unrendered() {
        let rows: &[(&str, &str)] = &[
            ("arithmetic, which is not a value of the call", "#set($a = 1 + 1)$a"),
            ("a list", "#set($a = [1,2])$a"),
            ("a member assignment", "#set($a.b = 1)$a"),
            ("an unterminated directive", "#set($a = 1 $a"),
            (
                "a name nothing bound, which is a variable outside the language",
                "$root.id",
            ),
            (
                "a binding of a reference nothing resolves",
                "#set($a = $whatever.body)$a",
            ),
        ];
        let inputs = inputs();
        for (case, template) in rows {
            let actual = rendered(template, &inputs);
            assert!(
                actual.is_err(),
                "case {case:?}: expected the template to be left unrendered, got {actual:?}"
            );
        }
    }

    /// RULE: a conditional directive renders the ONE branch its condition chooses — and the
    /// branches it does not choose render nothing and resolve nothing.
    #[test]
    fn a_conditional_renders_the_branch_its_condition_chooses() {
        /// (case, the template, what it renders to)
        type Row<'a> = (&'a str, &'a str, &'a str);
        let rows: &[Row<'_>] = &[
            (
                "the comparison the recorded fixture is written with, taking the else branch",
                "#set($d = $input.path('$.qty'))#if($d < 0)[low]#else[$d]#end",
                "[2]",
            ),
            (
                "the same comparison taking the if branch",
                "#set($d = $input.path('$.qty'))#if($d > 0)[$d]#else[low]#end",
                "[2]",
            ),
            (
                "a quoted number against a number, which the language does not equate",
                r#"#set($d = $input.json('$.qty'))#if($d == 2)[two]#else[other]#end"#,
                "[other]",
            ),
            (
                "the same quoted number against its own text, which it does equate",
                r#"#set($d = $input.json('$.qty'))#if($d == '2')[two]#else[other]#end"#,
                "[two]",
            ),
            (
                "an equality of two texts",
                "#if($input.params('tenant') == 'acme')[ours]#else[theirs]#end",
                "[ours]",
            ),
            (
                "a reference this call has a value for, which is true",
                "#if($input.params('tenant'))[named]#else[unnamed]#end",
                "[named]",
            ),
            (
                "a reference this call has no value for, which is false",
                "#if($input.params('absent') != '')[named]#else[unnamed]#end",
                "[unnamed]",
            ),
            (
                "a negation",
                "#if(!$input.params('tenant'))[no]#else[yes]#end",
                "[yes]",
            ),
            (
                "an elseif, whose branch is the one taken",
                "#set($d = $input.path('$.qty'))#if($d > 5)[big]#elseif($d > 1)[some]#else[none]#end",
                "[some]",
            ),
            (
                "an elseif after a branch already taken, which renders nothing",
                "#if(true)[first]#elseif(true)[second]#else[third]#end",
                "[first]",
            ),
            (
                "a conjunction",
                "#set($d = $input.path('$.qty'))#if($d > 1 && $d < 3)[inside]#else[outside]#end",
                "[inside]",
            ),
            (
                "a disjunction",
                "#set($d = $input.path('$.qty'))#if($d < 0 || $d == 2)[either]#else[neither]#end",
                "[either]",
            ),
            (
                "a conditional nested in the branch that is taken",
                "#if(true)[#if(false)a#else b#end]#end",
                "[ b]",
            ),
            (
                "a reference inside the branch NOT taken, which is never resolved",
                "#if(false)$whatever.nothing#else[served]#end",
                "[served]",
            ),
            (
                "a binding inside the branch not taken, which never happens",
                "#if(false)#set($a = 'no')#end#if(true)[kept]#end",
                "[kept]",
            ),
            (
                "the directives on lines of their own, whose line breaks render nothing",
                "#if(true)\n{\"a\":1}\n#end\n",
                "{\"a\":1}\n",
            ),
        ];
        let inputs = inputs();
        for (case, template, expected) in rows {
            let actual = rendered(template, &inputs);
            assert_eq!(
                actual.as_deref().ok(),
                Some(*expected),
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a conditional whose directives do not pair up leaves the template unrendered,
    /// rather than rendering a piece of it — and so does one that nests, or asks for work,
    /// past the bounds this emulator renders under.
    #[test]
    fn an_unbalanced_conditional_leaves_the_template_unrendered() {
        let rows: &[(&str, &str)] = &[
            ("a #if nothing closes", "#if(true)[a]"),
            ("a #end that closes nothing", "[a]#end"),
            ("a #else that closes nothing", "[a]#else[b]"),
            ("a #elseif that closes nothing", "[a]#elseif(true)[b]"),
        ];
        let inputs = inputs();
        for (case, template) in rows {
            let actual = rendered(template, &inputs);
            assert!(
                actual.is_err(),
                "case {case:?}: expected the template to be left unrendered, got {actual:?}"
            );
        }
        // OVERSIZED, built rather than written out: conditionals nested past the depth this
        // emulator performs, and a `#set` of the payload repeated past the reads one
        // rendering may do. Neither recurses on the text's own nesting.
        let nested = format!(
            "{}[a]{}",
            "#if(true)".repeat(MAX_BRANCH_DEPTH + 1),
            "#end".repeat(MAX_BRANCH_DEPTH + 1)
        );
        let refused = rendered(&nested, &inputs);
        assert!(
            refused.is_err(),
            "case \"conditionals nested past {MAX_BRANCH_DEPTH}\": expected the template to \
             be left unrendered, got {refused:?}"
        );
        let payload = inputs.payload.as_deref().unwrap_or_default().len();
        let bindings = MAX_RENDER_WORK / payload + 2;
        let repeated = "#set($root = $input.path('$'))".repeat(bindings);
        let refused = rendered(&repeated, &inputs);
        assert!(
            refused.is_err(),
            "case \"{bindings} bindings of the payload\": expected the rendering to be \
             refused past {MAX_RENDER_WORK} bytes read, got {refused:?}"
        );
        // A binding grown by COPYING is charged what it copies, so a template that doubles
        // one runs the same budget down rather than holding copies for free.
        let doubled = format!(
            "#set($a = \"$input.body\"){}",
            "#set($a = \"$a$a\")".repeat(32)
        );
        let refused = rendered(&doubled, &inputs);
        assert!(
            refused.is_err(),
            "case \"a binding doubled 32 times\": expected the rendering to be refused, got \
             {} bytes",
            refused.map(|body| body.len()).unwrap_or_default()
        );
        // And a reference to a MEMBER of a bound document costs that member rather than the
        // document, so a template that names one many times still renders: the budget bounds
        // what is copied, not how often a binding is read.
        let many = format!(
            "#set($root = $input.path('$')){}",
            "$root.id".repeat(1_000)
        );
        let rendered_many = rendered(&many, &inputs);
        assert_eq!(
            rendered_many.as_deref().ok().map(str::len),
            Some(2_000),
            "case \"a thousand references to a bound member\": expected the member rendered \
             each time, got {rendered_many:?}"
        );
        // And the nesting this emulator DOES perform renders, so the bound is a ceiling
        // rather than a refusal of every nested conditional.
        let inside = format!(
            "{}[a]{}",
            "#if(true)".repeat(MAX_BRANCH_DEPTH),
            "#end".repeat(MAX_BRANCH_DEPTH)
        );
        let rendered = rendered(&inside, &inputs);
        assert_eq!(
            rendered.as_deref().ok(),
            Some("[a]"),
            "case \"conditionals nested to {MAX_BRANCH_DEPTH}\": expected [a], got \
             {rendered:?}"
        );
    }

    /// RULE: `$util.escapeJavaScript` replaces exactly the characters a JavaScript string
    /// literal cannot carry as they stand, each with the escape that grammar names.
    #[test]
    fn the_javascript_escape_replaces_what_a_string_literal_cannot_carry() {
        /// (case, the text, its escaping)
        type Row<'a> = (&'a str, &'a str, &'a str);
        let rows: &[Row<'_>] = &[
            ("text needing no escape at all", "a-b_c 1", "a-b_c 1"),
            ("a quoted document", r#"{"a":1}"#, r#"{\"a\":1}"#),
            ("an apostrophe", "it's", r"it\'s"),
            ("a backslash", r"a\b", r"a\\b"),
            ("a forward slash", "a/b", r"a\/b"),
            ("the named control escapes", "a\tb\nc\rd", r"a\tb\nc\rd"),
            ("a control character the grammar spells", "a\u{1}b", r"a\u0001b"),
            ("text past ASCII, which needs none", "café", "café"),
            ("nothing at all", "", ""),
        ];
        for (case, text, expected) in rows {
            let actual = escaped_as_javascript(text);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: `$input.params()` names the map of this call's method request parameters keyed
    /// by the source each came from, and a reference reading into it — a source and then a
    /// name, by either spelling of a map's key — resolves to that parameter's value.
    #[test]
    fn the_parameter_map_names_each_parameter_by_the_source_it_came_from() {
        /// (case, the template, what it renders to)
        type Row<'a> = (&'a str, &'a str, &'a str);
        // The rows differ along the SOURCE the parameter came from, the SPELLING of each
        // step (a member, `get(…)`, the braced reference), the CASE a header name is written
        // in, whether the map is read directly or bound by a `#set` first, and whether the
        // name is one this call carries at all.
        let rows: &[Row<'_>] = &[
            (
                "the path parameter, by get",
                "$input.params().path.get('pet')",
                "fido",
            ),
            (
                "the path parameter, by the bare member",
                "$input.params().path.pet",
                "fido",
            ),
            (
                "the query-string parameter",
                "$input.params().querystring.get('tenant')",
                "acme",
            ),
            (
                "the header, by the name as sent",
                "$input.params().header.get('X-Trace')",
                "t-1",
            ),
            (
                "the header, by another case of the same name",
                "$input.params().header.get('x-trace')",
                "t-1",
            ),
            (
                "the source named by get as well",
                "$input.params().get('path').get('pet')",
                "fido",
            ),
            (
                "the braced spelling of the whole reference",
                "${input.params().path.get('pet')}",
                "fido",
            ),
            (
                "the map bound by a #set and read through the binding",
                "#set($all = $input.params())$all.path.pet",
                "fido",
            ),
            (
                "the value carried into a conversion",
                "$util.urlEncode($input.params().path.get('pet'))",
                "fido",
            ),
            (
                "a name the source does not carry, which is an absent value",
                "$input.params().path.get('absent')",
                "",
            ),
            (
                "a key the map does not carry at all",
                "$input.params().elsewhere.get('pet')",
                "",
            ),
            (
                "the same parameter named with no source at all, which is the one-argument \
                 form",
                "$input.params('pet')",
                "fido",
            ),
        ];
        let inputs = inputs();
        for (case, template, expected) in rows {
            let rendered = rendered(template, &inputs);
            assert_eq!(
                rendered.as_deref().ok(),
                Some(*expected),
                "case {case:?}: expected {expected:?}, got {rendered:?}"
            );
        }
    }

    #[test]
    fn a_reference_nothing_resolves_leaves_the_template_unrendered() {
        let cases: &[&str] = &[
            // A variable outside the documented set, and a transformation this emulator
            // states rather than performs.
            "$util.parseJson($input.body)",
            "$whatever.body",
            // A member of a documented variable that this call has no value for.
            "$context.status",
            "$stageVariables.undefined",
            // A method the variable does not document, and one called wrongly.
            "$input.query('$.id')",
            "$input.params('a', 'b')",
            // A reference whose value is a MAP or a structure rather than one value: the
            // whole parameter map, one source of it, and a path selecting a structure or
            // several nodes.
            "$input.params()",
            "$input.params().path",
            // A key BEYOND a parameter's own name, which is text and carries none.
            "$input.params().path.pet.deeper",
            "$input.path('$.nested')",
            "$input.json('$.*')",
            // A path carrying a construct whose work is per node rather than per document
            // — a filter, a descendant segment, a wildcard, a selector list, a slice — one
            // that is not rooted at the payload, and one that is not a path at all: the
            // single-value reader evaluates none of them.
            "$input.json('$[?@.id == \"a1\"]')",
            "$input.json('$..deep')",
            "$input.json('$..nested..deep')",
            "$input.json('$[0,0]')",
            "$input.json('$[0:2]')",
            "$input.json('$[*]')",
            "$input.json('$.*')",
            "$input.json('id')",
            "$input.json('@.id')",
            // A reference this module cannot take apart at all: an unterminated brace, an
            // unterminated argument, an argument in neither documented form, and
            // arguments nested past the bound this reading is done under.
            "${input.params('tenant')",
            "$input.params('tenant",
            "$input.params(tenant)",
            "$a.b($a.b($a.b($a.b($a.b($a.b($a.b($a.b($a.b($a.b('x')))))))))",
        ];
        let inputs = inputs();
        for template in cases {
            let rendered = rendered(template, &inputs);
            assert!(
                rendered.is_err(),
                "case {template:?}: expected the template to be left unrendered, got \
                 {rendered:?}"
            );
        }
        // And a path the single-value reader does not evaluate because of its SIZE: more
        // segments than it reads, built rather than written out.
        let deep = format!(
            "$input.json('${}')",
            ".a".repeat(::service_common::json_reference_path::MAX_SEGMENTS + 1)
        );
        let stepped = rendered(&deep, &inputs);
        assert!(
            stepped.is_err(),
            "case a path past the segments the reader evaluates: expected the template to \
             be left unrendered, got {stepped:?}"
        );
    }

    #[test]
    fn only_the_reference_path_forms_are_applied_to_the_payload() {
        // The forms a mapping template's documented path takes are the REFERENCE paths
        // `service_common`'s single-value reader evaluates, and the constructs it refuses
        // are the ones whose work is per node rather than per document. Asserted through
        // the rendering, which is what this module's cost argument rests on: the rows
        // differ along the step kind (member, index, quoted member), how many steps are
        // chained, and which construct puts a path outside the set.
        let inputs = inputs();
        let applied: &[(&str, &str)] = &[
            ("$input.json('$.id')", "\"a1\""),
            ("$input.json('$.nested.deep')", "\"value\""),
            (r#"$input.json("$['id']")"#, "\"a1\""),
            ("$input.path('$.nested.deep')", "value"),
            // A place the payload does not carry: evaluated, and selected nothing.
            ("$input.json('$.absent')", ""),
            ("$input.json('$.nested.absent')", ""),
        ];
        for (template, expected) in applied {
            let rendered = rendered(template, &inputs);
            assert_eq!(
                rendered.as_deref().ok(),
                Some(*expected),
                "case {template:?}: expected {expected:?}, got {rendered:?}"
            );
        }
    }

    #[test]
    fn a_rendering_past_the_emulators_own_bounds_leaves_the_template_unrendered() {
        // Two bounds, each on the PRODUCT of the references a template carries and the
        // payload one of them names: the BODY it produces, and the bytes it READS. A small
        // template must not be able to amplify one request into an unbounded one either
        // way.
        let inputs = inputs();
        let payload = inputs.payload.as_deref().unwrap_or_default().len();
        // The body: every reference writes the payload out again.
        let references = MAX_RENDERED_BYTES / payload + 2;
        let template = "$input.body".repeat(references);
        let refused = rendered(&template, &inputs);
        assert!(
            refused.is_err(),
            "case {references} payload references: expected the rendering to be refused \
             past {MAX_RENDERED_BYTES} bytes, got {} bytes",
            refused.map(|body| body.len()).unwrap_or_default()
        );
        // The reads: every reference queries the payload, and writes almost nothing.
        let queries = MAX_RENDER_WORK / payload + 2;
        let template = "$input.json('$.qty')".repeat(queries);
        let refused = rendered(&template, &inputs);
        assert!(
            refused.is_err(),
            "case {queries} payload queries: expected the rendering to be refused past \
             {MAX_RENDER_WORK} bytes read, got {} bytes",
            refused.map(|body| body.len()).unwrap_or_default()
        );
        // And one that stays inside both renders.
        let template = "$input.body".repeat(2);
        let twice = rendered(&template, &inputs);
        assert_eq!(
            twice.as_deref().ok().map(str::len),
            Some(payload * 2),
            "case two payload references: expected {} characters, got {twice:?}",
            payload * 2
        );
    }

    #[test]
    fn a_payload_that_is_not_text_leaves_a_payload_reference_unrendered() {
        // `$input.body` stands for the payload AS TEXT, so a payload that is not text is
        // left unrendered rather than written out with its undecodable bytes replaced.
        let inputs = TemplateInputs {
            payload: Some(vec![0x7b, 0x22, 0xff, 0xfe, 0x22, 0x7d]),
            ..inputs()
        };
        let refused = rendered("$input.body", &inputs);
        assert!(
            refused.is_err(),
            "case a non-text payload: expected the template to be left unrendered, got \
             {refused:?}"
        );
        // The parameters of the same call are still text to read.
        let parameter = rendered("$input.params('tenant')", &inputs);
        assert_eq!(
            parameter.as_deref().ok(),
            Some("acme"),
            "case params beside a non-text payload: expected acme, got {parameter:?}"
        );
    }

    #[test]
    fn a_payload_reference_is_unresolved_where_the_call_carries_no_payload_to_read() {
        let inputs = TemplateInputs {
            payload: None,
            ..inputs()
        };
        for template in ["$input.body", "$input.json('$.id')", "$input.path('$.id')"] {
            let rendered = rendered(template, &inputs);
            assert!(
                rendered.is_err(),
                "case {template:?}: expected no payload to leave the template unrendered, \
                 got {rendered:?}"
            );
        }
        // The parameters of the same call are still there to read.
        let rendered = rendered("$input.params('tenant')", &inputs);
        assert_eq!(
            rendered.as_deref().ok(),
            Some("acme"),
            "case params without a payload: expected acme, got {rendered:?}"
        );
    }

    /// RULE: an override directive states the parameter it names, with the value its
    /// expression comes to, renders nothing itself, and keeps the FIRST value stated for a
    /// parameter it names twice. The response's STATUS is the one member it states that takes
    /// a number: an expression coming to a value of any other type states no status at all.
    #[test]
    fn an_override_states_the_parameter_it_names() {
        /// (case, the template, the text it renders to, the parameters it stated as
        /// `side location name=value` lines)
        type Row<'a> = (&'a str, &'a str, &'a str, &'a [&'a str]);
        let rows: &[Row<'_>] = &[
            (
                "a static header value",
                "#set($context.requestOverride.header.x-flow = 'on')body",
                "body",
                &["request header x-flow=on"],
            ),
            (
                "a value read off the call",
                "#set($context.requestOverride.querystring.tenant = $input.params('tenant'))",
                "",
                &["request querystring tenant=acme"],
            ),
            (
                "a value read off a binding",
                "#set($r = $input.path('$'))#set($context.requestOverride.path.id = $r.id)",
                "",
                &["request path id=a1"],
            ),
            (
                "a number, which a parameter carries as the text it spells",
                "#set($context.responseOverride.status = 400)",
                "",
                &["response status=400"],
            ),
            (
                "a double-quoted literal spelling a status, which states no status",
                "#set($context.responseOverride.status = \"201\")",
                "",
                &[],
            ),
            (
                "a single-quoted literal spelling a status, which states no status",
                "#set($context.responseOverride.status = '503')",
                "",
                &[],
            ),
            (
                "a status read off the call as text, which states no status",
                "#set($context.responseOverride.status = $input.params('tenant'))",
                "",
                &[],
            ),
            (
                "a header whose value is a quoted number, which a parameter carries as text",
                "#set($context.responseOverride.header.x-code = \"201\")",
                "",
                &["response header x-code=201"],
            ),
            (
                "the same parameter stated twice, which keeps the first value",
                "#set($context.requestOverride.header.x-flow = 'first')\
                 #set($context.requestOverride.header.x-flow = 'second')",
                "",
                &["request header x-flow=first"],
            ),
            (
                "the same status stated twice",
                "#set($context.responseOverride.status = 400)\
                 #set($context.responseOverride.status = 500)",
                "",
                &["response status=400"],
            ),
            (
                "both sides at once, and a header of each",
                "#set($context.requestOverride.header.a = '1')\
                 #set($context.responseOverride.header.b = '2')",
                "",
                &["request header a=1", "response header b=2"],
            ),
            (
                "an override inside a branch this call does not take, which states nothing",
                "#if($context.stage == 'other')\
                 #set($context.responseOverride.status = 500)\
                 #end",
                "",
                &[],
            ),
            (
                "an override inside the branch this call does take",
                "#if($context.stage == 'test')\
                 #set($context.responseOverride.status = 404)\
                 #end",
                "",
                &["response status=404"],
            ),
        ];
        let inputs = inputs();
        for (case, template, text, stated) in rows {
            let actual = super::rendered(template, &inputs);
            let Ok(actual) = actual else {
                panic!("case {case:?}: expected the template to render, got {actual:?}");
            };
            assert_eq!(
                actual.text, *text,
                "case {case:?}: expected the text {text:?}, got {:?}",
                actual.text
            );
            let mut lines = Vec::new();
            for (side, overrides) in [("request", &actual.request), ("response", &actual.response)]
            {
                for (location, parameters) in [
                    ("header", &overrides.header),
                    ("querystring", &overrides.querystring),
                    ("path", &overrides.path),
                ] {
                    for (name, value) in parameters {
                        lines.push(format!("{side} {location} {name}={value}"));
                    }
                }
                if let Some(status) = &overrides.status {
                    lines.push(format!("{side} status={status}"));
                }
            }
            assert_eq!(
                lines,
                stated.iter().map(|line| (*line).to_string()).collect::<Vec<String>>(),
                "case {case:?}: expected the overrides {stated:?}, got {lines:?}"
            );
        }
    }
}
