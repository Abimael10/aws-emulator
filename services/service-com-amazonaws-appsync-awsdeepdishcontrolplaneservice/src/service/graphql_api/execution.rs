// SPDX-License-Identifier: CC0-1.0

//! Executing one GraphQL request against a GraphQL API's own resolvers: the work behind the
//! API's `GRAPHQL` endpoint ([`super::query`]).
//!
//! A GraphQL API is not a document that only gets read back. A resolver is "attached" to one
//! field of one type of its API and carries "the `resolver` code that contains the request
//! and response functions" (`Resolver$code`), run by the APPSYNC_JS runtime; so a request
//! that selects that field is answered by what the code RETURNS, under the field's response
//! key, and by nothing else.
//!
//! What this module does with one request, in order:
//!
//! 1. The DOCUMENT is parsed by the maintained GraphQL parser this crate already reads a
//!    schema with (`apollo-parser`) — never by anything hand-written — and the operation to
//!    run is the one the request named, or the document's only one. Its selected fields are
//!    read with their aliases, their arguments (the request's own `variables` substituted
//!    for every variable) and their sub-selections, with a fragment spread and an inline
//!    fragment spliced in where the document uses one.
//! 2. Each field is resolved against the record the control plane wrote: the resolver
//!    attached to that field of the type the field belongs to, the data source it names, and
//!    — for a `PIPELINE` resolver — the functions it invokes, in order. A field that carries
//!    no resolver takes its value from the object its parent resolved to, which is what a
//!    GraphQL object's own member is.
//! 3. The DEFINITION is RUN. A resolver defined by `code` is run by the same engine embedded
//!    in this process that `EvaluateCode` evaluates a resolver with
//!    ([`crate::ops::evaluate_code::appsync_js`]): the `request` function over the field's
//!    `ctx`, then the `response` function over the result the data source produced. Every
//!    bound that module declares holds here — a wall clock, a step budget, memory and stack
//!    ceilings and an answer ceiling — and each run gets a fresh engine, so nothing of one
//!    field's run is reachable from the next. A resolver defined by MAPPING TEMPLATES is run
//!    the same way round by the template runtime ([`super::mapping_template`]): the request
//!    template renders the configuration the data source is called with, and the response
//!    template renders the value the field takes.
//! 4. The sub-selections of each resolved value are PROJECTED onto it, as a GraphQL result
//!    carries exactly the fields the request selected.
//!
//! A field whose code raised, and one this emulator cannot resolve at all, is reported as an
//! error of that field's own path with the value `null`, which is how a GraphQL result
//! states a field that did not resolve.

use ::std::time::Duration;

use ::serde_json::{json, Map as JsonMap, Value as Json};

use super::mapping_template::{self, Raised};
use super::PlaneContext;
use crate::ops::evaluate_code::appsync_js::{self, Outcome};
use crate::state::{DataSourceRecord, GraphqlApiRecord, ResolverRecord};

/// The GraphQL request a client POSTs: "a JSON object with a `query`", with the operation to
/// run and the variables its document is parameterised by.
#[derive(Debug, Clone, Default)]
pub struct GraphqlRequest {
    /// The GraphQL document.
    pub query: String,
    /// The operation within it to run, when the request names one.
    pub operation_name: Option<String>,
    /// The variables the document's own variable definitions take their values from.
    pub variables: JsonMap<String, Json>,
    /// The request's own fields, which a resolver reads as `ctx.request.headers`.
    pub headers: JsonMap<String, Json>,
    /// Who the request presented itself as, which a resolver reads as `ctx.identity` —
    /// `Json::Null` for a request whose authorization mode names no principal
    /// ([`super::query`] builds it).
    pub identity: Json,
    /// The `(typeName, fieldName)` pairs the caller may NOT access: the fields the
    /// authorization in front of the API refused ([`super::query`]), which resolve to
    /// nothing and are reported as errors of their own path instead of being run.
    pub unauthorized: Vec<(String, String)>,
}

/// What executing one request came to: the data it resolved, and the errors of the fields
/// that did not resolve.
#[derive(Debug, Clone, Default)]
pub struct Answer {
    /// The selected data, or `None` for a request no operation of which was executed at all
    /// — which is what a GraphQL result reports with no `data` member.
    pub data: Option<Json>,
    /// One entry per field that did not resolve, and the request-level faults that stopped
    /// the execution before any field ran.
    pub errors: Vec<Json>,
    /// What resolving each field came to, for the API's own field logging
    /// ([`super::field_logs`]): empty for an API that captures no field-level logs, which is
    /// what the execution is told before it starts.
    pub traces: Vec<FieldTrace>,
}

/// What resolving ONE field came to, as the API's field-level logging reports it: where the
/// field sits, what resolved it, how long that took, and the request and response each step
/// of the resolution GENERATED.
///
/// Collected only for an API whose `logConfig` captures field-level logs — the `fieldLogLevel`
/// rule ([`super::field_logs`]) — so an API that logs none pays nothing for it.
#[derive(Debug, Clone)]
pub struct FieldTrace {
    /// Where in the result the field sits, as a GraphQL error's own `path` names it.
    pub path: Vec<Json>,
    /// The type the field belongs to.
    pub parent_type: String,
    /// The field's own name, which is its response key's name before any alias.
    pub field_name: String,
    /// The name of the type the field resolves to, as the API's schema declares it, or the
    /// empty name when it declares none.
    pub return_type: String,
    /// The ARN of the resolver attached to the field, or `None` for a field that carries
    /// none.
    pub resolver_arn: Option<String>,
    /// How long after the execution started this field began resolving.
    pub start_offset: Duration,
    /// How long resolving it took.
    pub duration: Duration,
    /// The sentence the field did not resolve with, or `None` for one that resolved.
    pub error: Option<String>,
    /// The `ctx` the field was resolved with — the arguments, the source and the field's
    /// `info` — which is the CONTEXT a verbose field log carries.
    pub context: Json,
    /// One entry per step that resolved the field, in the order they ran: the request each
    /// step's own `request` function generated and the response its `response` function
    /// returned.
    pub steps: Vec<GeneratedStep>,
}

/// The request and response ONE step of a resolution generated: "the generated
/// request/response functions that got resolved" for the field (`LogConfig$fieldLogLevel`).
#[derive(Debug, Clone)]
pub struct GeneratedStep {
    /// What the step is called: the field for a UNIT resolver, the function's name for a
    /// step of a pipeline.
    pub name: String,
    /// The request the step's `request` function generated, or `None` when it did not
    /// return one.
    pub request: Option<Json>,
    /// What the step's `response` function returned, or `None` when it did not return.
    pub response: Option<Json>,
}

impl Answer {
    /// The answer that reports one request-level fault and no data.
    fn refused(message: String) -> Self {
        Self {
            data: None,
            errors: vec![json!({"message": message})],
            traces: Vec::new(),
        }
    }

    /// This answer as the body a GraphQL response carries: `data` where the execution
    /// produced any, and `errors` where anything did not resolve.
    pub fn document(&self) -> Json {
        let mut body = JsonMap::new();
        if let Some(data) = &self.data {
            body.insert(DATA_MEMBER.to_string(), data.clone());
        }
        if !self.errors.is_empty() {
            body.insert(ERRORS_MEMBER.to_string(), Json::Array(self.errors.clone()));
        }
        Json::Object(body)
    }
}

/// The members a GraphQL response carries.
const DATA_MEMBER: &str = "data";
const ERRORS_MEMBER: &str = "errors";

/// How deep a selection set this endpoint executes.
///
/// An emulator resource guard, NOT an AWS quota: every level of a selection is work the
/// REQUEST chooses, and a resolved field may be resolved again at the next level, so the
/// descent is bounded in host code rather than by what the document asks for.
const MAX_SELECTION_DEPTH: usize = 16;

/// How many resolvers one request may run.
///
/// An emulator resource guard, NOT an AWS quota, for the same reason as the depth: each run
/// is a fresh engine, so the number of them one request can ask for is bounded here.
const MAX_RESOLVER_RUNS: usize = 64;

/// How many fields one selection set may select, for the same reason.
const MAX_SELECTED_FIELDS: usize = 64;

/// How many FIELDS one request's own field-log trace keeps, how many steps of one field it
/// keeps, and how many bytes of their verbose halves — the field's context and the request and
/// response its steps generated — it RETAINS in all.
///
/// Emulator resource guards, for the reason every other bound here is one: the number of
/// fields a request resolves is what the document and the values its resolvers return choose,
/// and a trace holds request-derived values, so what the collection may hold is bounded in
/// host code rather than by the request. A request that resolves more fields than the first
/// bound keeps traces the ones it kept; one that would retain more than the byte bound keeps
/// each field's trace with the verbose halves left out, exactly as `excludeVerboseContent`
/// leaves them out, so the field is still reported.
const MAX_FIELD_TRACES: usize = 64;
const MAX_TRACED_STEPS: usize = 16;
const MAX_TRACE_BYTES: usize = 256 * 1024;

/// How many errors one result REPORTS, and how many bytes of argument one document's
/// variables may be substituted into.
///
/// Emulator resource guards of the same kind: a field is resolved once per value a resolver's
/// answer projects it over, so the number of errors a result can carry is not bounded by the
/// fields the document selects; and a variable's value is substituted into every argument
/// that references it, so what the selected arguments hold is not bounded by the document's
/// own size. A result past the first bound carries that many errors and one entry saying so;
/// a document past the second is refused before anything is resolved.
const MAX_FIELD_ERRORS: usize = 256;
const MAX_ARGUMENT_BYTES: usize = 4 * 1024 * 1024;

/// How many fields one request's whole document may expand to, every level and every
/// fragment splice together.
///
/// An emulator resource guard, NOT an AWS quota, and the one that bounds the EXPANSION
/// rather than one level of it: see [`Expansion`] for why the width and the depth bounds do
/// not, and why a document past this is refused before anything is built.
const MAX_SELECTED_NODES: usize = 1_024;

/// How long ALL the mapping templates of ONE request may run, between them: the evaluations
/// of one request share this, so a request selecting many template-defined fields cannot
/// hold a thread for the sum of their own budgets.
const MAX_TEMPLATE_RUNTIME: ::std::time::Duration = ::std::time::Duration::from_secs(2);

/// How long the CALLER waits for one evaluation, past the budget that evaluation was given:
/// the margin it takes a run to notice its own clock, held outside the run as the APPSYNC_JS
/// evaluations of this service hold theirs.
const TEMPLATE_WAIT_CEILING: ::std::time::Duration = ::std::time::Duration::from_secs(5);

/// The `DataSourceType` of a data source AppSync resolves against WITHOUT leaving itself:
/// `NONE` is "There is no data source", so the request function's own `payload` is the
/// result the response function is handed ("local resolvers" in the AppSync developer
/// guide).
const LOCAL_DATA_SOURCE: &str = "NONE";

/// The member a request function returns the result of a local resolver in.
const PAYLOAD_MEMBER: &str = "payload";

/// The names of the two functions a resolver's `code` defines (`Resolver$code`: "the
/// `resolver` code that contains the request and response functions").
const REQUEST_FUNCTION: &str = "request";
const RESPONSE_FUNCTION: &str = "response";

/// Execute `request` against the API `context` serves.
///
/// The store is read once per field, never across a run of the engine: a resolver's
/// definition is copied out under the lock and the code runs with the lock released, so one
/// field's run cannot hold up every other request to this service.
pub async fn execute(context: &PlaneContext, request: &GraphqlRequest) -> Answer {
    // The parse happens in a scope of its own: the parser's own tree is not `Send`, so what
    // leaves this call is the operation and the fields it selects, read out of that tree and
    // owned, and the tree itself is gone before anything is awaited.
    let (kind, fields) = match read_operation(request) {
        Ok(read) => read,
        Err(refusal) => return Answer::refused(refusal),
    };
    // The type the operation's fields belong to: the type the API's schema binds as that
    // operation's root, which is what says WHICH resolvers the selection addresses.
    let Some(root_type) = context
        .state
        .lock()
        .graphql_apis
        .get(&context.api_id)
        .map(|api| match kind {
            OperationKind::Query => api.query_root_type(),
            OperationKind::Mutation => api.mutation_root_type(),
            OperationKind::Subscription => api.subscription_root_type(),
        })
    else {
        return Answer::refused(crate::state::API_NOT_FOUND_MESSAGE.to_string());
    };
    if kind == OperationKind::Subscription {
        return Answer::refused(SUBSCRIPTIONS_ARE_REALTIME.to_string());
    }
    let mut execution = Execution {
        context,
        request,
        runs: 0,
        errors: Vec::new(),
        started: ::std::time::Instant::now(),
        traced: super::field_logs::captures_field_logs(&context.state.lock(), &context.api_id),
        traces: Vec::new(),
        trace_bytes: MAX_TRACE_BYTES,
        steps: Vec::new(),
        appended: Vec::new(),
        template_deadline: ::std::time::Instant::now() + MAX_TEMPLATE_RUNTIME,
    };
    let data = execution
        .resolve_fields(&root_type, &Json::Null, &fields, Vec::new(), 0)
        .await;
    Answer {
        data: Some(data),
        errors: execution.errors,
        traces: execution.traces,
    }
}

/// The fields one request selects of the operation's ROOT type: the `(typeName, fieldName)`
/// pairs a call on this API addresses, which is what an IAM policy about the API's fields is
/// judged against ([`super::query`]).
///
/// Empty for a request this endpoint executes nothing of — one whose document does not
/// parse, or that names no operation of it — because such a request addresses no field; the
/// execution reports that fault itself.
pub fn selected_root_fields(
    context: &PlaneContext,
    request: &GraphqlRequest,
) -> Vec<(String, String)> {
    let Ok((kind, fields)) = read_operation(request) else {
        return Vec::new();
    };
    let Some(root_type) = context
        .state
        .lock()
        .graphql_apis
        .get(&context.api_id)
        .map(|api| match kind {
            OperationKind::Query => api.query_root_type(),
            OperationKind::Mutation => api.mutation_root_type(),
            OperationKind::Subscription => api.subscription_root_type(),
        })
    else {
        return Vec::new();
    };
    fields
        .iter()
        .map(|field| (root_type.clone(), field.name.clone()))
        .collect()
}

/// Read `request`'s document: the operation to run and the fields it selects, owned, with
/// the request's own variables already substituted.
///
/// Synchronous on purpose — nothing of the parser's tree escapes it.
fn read_operation(request: &GraphqlRequest) -> Result<(OperationKind, Vec<SelectedField>), String> {
    let parsed = ::apollo_parser::Parser::new(&request.query)
        .recursion_limit(crate::state::DEFINITION_RECURSION_LIMIT)
        .token_limit(crate::state::DEFINITION_TOKEN_LIMIT)
        .parse();
    if let Some(error) = parsed.errors().next() {
        return Err(format!("{NOT_A_DOCUMENT} {}", error.message()));
    }
    let document = parsed.document();
    let operation = selected_operation(&document, request.operation_name.as_deref())
        .ok_or_else(|| no_such_operation(request.operation_name.as_deref()))?;
    let mut expansion = Expansion {
        document: &document,
        variables: &request.variables,
        remaining: MAX_SELECTED_NODES,
        held: MAX_ARGUMENT_BYTES,
        spreading: Vec::new(),
    };
    let fields = expansion.selection_set(operation.selection_set.as_ref(), 0)?;
    Ok((operation.kind, fields))
}

/// One request being executed: what it is executing against, and what it has produced so
/// far.
struct Execution<'a> {
    context: &'a PlaneContext,
    request: &'a GraphqlRequest,
    /// How many resolvers this request has run, which [`MAX_RESOLVER_RUNS`] bounds.
    runs: usize,
    /// The errors of the fields that did not resolve, in the order they were resolved.
    errors: Vec<Json>,
    /// When this execution began, which every field's own offset is measured from.
    started: ::std::time::Instant,
    /// Whether the API captures field-level logs at all: a field's trace is collected only
    /// then, so an API that logs none pays nothing for the collection.
    traced: bool,
    /// What resolving each field came to, in the order the fields were resolved.
    traces: Vec<FieldTrace>,
    /// How many more bytes of the verbose halves of those traces this request may retain,
    /// which [`MAX_TRACE_BYTES`] starts at.
    trace_bytes: usize,
    /// The steps of the field being resolved RIGHT NOW, which each step appends its
    /// generated request and its response to and the field's own trace then takes.
    steps: Vec<GeneratedStep>,
    /// The errors the mapping templates of the field being resolved APPENDED
    /// (`$util.appendError`, which "does not interrupt the evaluation"), drained onto
    /// [`Self::errors`] under that field's own path once it is resolved.
    appended: Vec<Raised>,
    /// When this request's mapping templates have had all the time they get between them:
    /// each evaluation is given what is left of it, so the PRODUCT of the fields a request
    /// selects and the templates each one runs is bounded, not just each run.
    template_deadline: ::std::time::Instant,
}

impl Execution<'_> {
    /// Resolve every field of one selection set against `source`, the value the parent field
    /// resolved to, and answer the object a GraphQL result carries them in.
    ///
    /// Boxed, because a field's own sub-selection is resolved the same way: the recursion is
    /// over a future, which Rust needs a pointer for.
    fn resolve_fields<'f>(
        &'f mut self,
        type_name: &'f str,
        source: &'f Json,
        fields: &'f [SelectedField],
        path: Vec<Json>,
        depth: usize,
    ) -> ::std::pin::Pin<Box<dyn ::std::future::Future<Output = Json> + Send + 'f>> {
        Box::pin(async move {
            let mut resolved = JsonMap::new();
            for field in fields {
                let mut field_path = path.clone();
                field_path.push(json!(field.key));
                let value = self
                    .resolve_field(type_name, source, field, &field_path, depth)
                    .await;
                resolved.insert(field.key.clone(), value);
            }
            Json::Object(resolved)
        })
    }

    /// Resolve ONE field: run the resolver attached to it where the API holds one, take the
    /// member of the parent object where it does not, and project the field's own
    /// sub-selection onto whatever that came to.
    async fn resolve_field(
        &mut self,
        type_name: &str,
        source: &Json,
        field: &SelectedField,
        path: &[Json],
        depth: usize,
    ) -> Json {
        if depth >= MAX_SELECTION_DEPTH {
            self.fault(field_error(path, &format!("{TOO_DEEP} {MAX_SELECTION_DEPTH}.")));
            return Json::Null;
        }
        // A field the authorization in front of the API refused this caller resolves to
        // nothing: no resolver of it runs, and the answer reports it as a field the caller
        // may not access.
        if self
            .request
            .unauthorized
            .iter()
            .any(|(refused_type, refused_field)| {
                refused_type == type_name && *refused_field == field.name
            })
        {
            self.fault(unauthorized_field(path, type_name, &field.name));
            return Json::Null;
        }
        let plan = self.plan(type_name, &field.name);
        let resolver_arn = plan.as_ref().and_then(|plan| plan.arn.clone());
        // Where this field sits in the execution, so its trace reports the offset and the
        // span the API's field logging states them as.
        let start_offset = self.started.elapsed();
        self.steps.clear();
        self.appended.clear();
        let resolved: Result<Json, Failure> = match plan {
            // A field the API attaches no resolver to is the member of the object its
            // parent resolved to, which is what a GraphQL object's own field is.
            None => Ok(source.get(&field.name).cloned().unwrap_or(Json::Null)),
            Some(plan) => {
                if self.runs >= MAX_RESOLVER_RUNS {
                    self.fault(field_error(
                        path,
                        &format!("{TOO_MANY_RUNS} {MAX_RESOLVER_RUNS}."),
                    ));
                    return Json::Null;
                }
                self.runs = self.runs.saturating_add(1);
                self.run(&plan, type_name, source, field).await
            }
        };
        if self.traced && self.traces.len() < MAX_FIELD_TRACES {
            let duration = self.started.elapsed().saturating_sub(start_offset);
            let return_type = self.field_type_name(type_name, &field.name);
            let steps = ::std::mem::take(&mut self.steps);
            // What this entry RETAINS is the verbose half of it — the field's own context and
            // the request and response its steps generated — and a request may retain only so
            // much of that ([`MAX_TRACE_BYTES`]). Past that, the field is still traced, with
            // the verbose sections left out exactly as `excludeVerboseContent` leaves them
            // out, so a request cannot choose how much of it one trace holds.
            let context = logged_context(type_name, field);
            let (context, steps) = match self.affordable(&context, &steps) {
                true => (context, steps),
                false => (Json::Null, Vec::new()),
            };
            self.traces.push(FieldTrace {
                path: path.to_vec(),
                parent_type: type_name.to_string(),
                field_name: field.name.clone(),
                return_type,
                resolver_arn,
                start_offset,
                duration,
                error: resolved.as_ref().err().map(Failure::message),
                context,
                steps,
            });
        }
        // Whatever the run came to, the errors its templates appended are reported
        // under this field's path: that is what appending one is for.
        for appended in ::std::mem::take(&mut self.appended) {
            self.fault(appended.document(path));
        }
        let value = match resolved {
            Ok(value) => value,
            Err(failure) => {
                self.fault(failure.document(path));
                return Json::Null;
            }
        };
        if field.selections.is_empty() {
            // A field with no sub-selection resolves to a SCALAR, which the type the schema
            // declares for it says how to write out.
            return match self.field_type(type_name, &field.name) {
                Some(declared) => json_coerced(&declared, value),
                None => value,
            };
        }
        // The field's own type is what says which resolvers its sub-selection addresses.
        let child_type = self.field_type_name(type_name, &field.name);
        self.projected(&child_type, value, field, path, depth).await
    }

    /// `value` with `field`'s sub-selection projected onto it: a list element by element, an
    /// object by its selected fields, and anything else as it stands — a scalar carries no
    /// sub-selection, and `null` resolves to `null`.
    ///
    /// A list counts as a level of its own, so a value nested in lists is held to the same
    /// descent bound a selection set is.
    fn projected<'f>(
        &'f mut self,
        child_type: &'f str,
        value: Json,
        field: &'f SelectedField,
        path: &'f [Json],
        depth: usize,
    ) -> ::std::pin::Pin<Box<dyn ::std::future::Future<Output = Json> + Send + 'f>> {
        Box::pin(async move {
            if depth >= MAX_SELECTION_DEPTH {
                self.fault(field_error(path, &format!("{TOO_DEEP} {MAX_SELECTION_DEPTH}.")));
                return Json::Null;
            }
            match value {
                Json::Array(elements) => {
                    let mut projected: Vec<Json> = Vec::with_capacity(elements.len());
                    for (index, element) in elements.into_iter().enumerate() {
                        let mut element_path = path.to_vec();
                        element_path.push(json!(index));
                        projected.push(
                            self.projected(
                                child_type,
                                element,
                                field,
                                &element_path,
                                depth.saturating_add(1),
                            )
                            .await,
                        );
                    }
                    Json::Array(projected)
                }
                Json::Object(_) => {
                    self.resolve_fields(
                        child_type,
                        &value,
                        &field.selections,
                        path.to_vec(),
                        depth.saturating_add(1),
                    )
                    .await
                }
                other => other,
            }
        })
    }

    /// What this API's records say resolves `field_name` of `type_name`, or `None` when no
    /// resolver is attached to that field.
    ///
    /// Copied out under ONE read of the store, so a resolver, the data source it names and
    /// the functions it invokes cannot be read from records that changed in between — and
    /// so that no guard is held while the engine runs.
    fn plan(&self, type_name: &str, field_name: &str) -> Option<ResolverPlan> {
        let store = self.context.state.lock();
        let api = store.graphql_apis.get(&self.context.api_id)?;
        let resolver = api.resolver(type_name, field_name)?;
        Some(ResolverPlan {
            arn: Some(resolver.arn.clone()),
            code: resolver.code.clone(),
            templates: Templates::of(
                resolver.request_mapping_template.as_deref(),
                resolver.response_mapping_template.as_deref(),
            ),
            data_source: step_data_source(api, resolver.data_source_name.as_deref()),
            functions: pipeline_steps(api, resolver),
            is_pipeline: resolver.kind == crate::state::PIPELINE_KIND,
        })
    }

    /// The name of the type `field_name` of `type_name` resolves to, or the empty name when
    /// the API's schema declares none — a field whose type is unknown addresses no
    /// resolver, so its sub-selection is taken from the value it resolved to.
    fn field_type_name(&self, type_name: &str, field_name: &str) -> String {
        let store = self.context.state.lock();
        store
            .graphql_apis
            .get(&self.context.api_id)
            .and_then(|api| api.types.get(type_name))
            .and_then(|record| {
                record
                    .parsed
                    .fields
                    .iter()
                    .find(|field| field.name == field_name)
            })
            .and_then(|field| field.value_type.as_ref().map(named_type))
            .unwrap_or_default()
    }

    /// The TYPE the API's schema declares for `field_name` of `type_name`, with everything it
    /// is wrapped in, or `None` when the schema declares none.
    fn field_type(&self, type_name: &str, field_name: &str) -> Option<crate::state::TypeRef> {
        let store = self.context.state.lock();
        store
            .graphql_apis
            .get(&self.context.api_id)
            .and_then(|api| api.types.get(type_name))
            .and_then(|record| {
                record
                    .parsed
                    .fields
                    .iter()
                    .find(|field| field.name == field_name)
            })
            .and_then(|field| field.value_type.clone())
    }

    /// Run `plan` for one field and answer the value the field resolves to.
    ///
    /// A UNIT resolver runs its own request step, resolves the result against the data source
    /// it names, and runs its response step over that result. A PIPELINE resolver runs its
    /// request step as the step BEFORE its functions, each function the same way in the order
    /// the pipeline names them — each handed the previous step's result as `ctx.prev.result`
    /// — and its response step as the step AFTER them. The stash is carried from step to
    /// step, as "a single stash object lives through a single resolver run".
    ///
    /// Each of those steps is the resolver's `code` where it is defined by code, and its
    /// mapping template for that direction where it is defined by templates.
    async fn run(
        &mut self,
        plan: &ResolverPlan,
        type_name: &str,
        source: &Json,
        field: &SelectedField,
    ) -> Result<Json, Failure> {
        let mut stash = Json::Object(JsonMap::new());
        let base = self.field_context(type_name, source, field);
        if !plan.is_pipeline {
            let step = PipelineStep {
                name: field.name.clone(),
                code: plan.code.clone(),
                templates: plan.templates.clone(),
                data_source: plan.data_source.clone(),
            };
            return self.run_step(&step, &base, &mut stash, &Json::Null).await;
        }
        // The pipeline's BEFORE step: what it produces is the `ctx.prev.result` the first
        // function is handed.
        let mut previous = match (&plan.code, &plan.templates) {
            (Some(code), _) => {
                returned(
                    code,
                    REQUEST_FUNCTION,
                    &step_context(&base, &stash, &Json::Null, None),
                    &mut stash,
                )
                .await?
            }
            (None, Some(templates)) => match &templates.request {
                Some(template) => {
                    let context = step_context(&base, &stash, &Json::Null, None);
                    let rendered = self.rendered(template, &context, &mut stash).await?;
                    read_rendered(&rendered, REQUEST_TEMPLATE)?
                }
                None => Json::Null,
            },
            (None, None) => Json::Null,
        };
        if plan.functions.is_empty() {
            return Err(NO_PIPELINE_FUNCTIONS.to_string().into());
        }
        for step in &plan.functions {
            previous = self
                .run_step(step, &base, &mut stash, &previous)
                .await
                .map_err(|failure| failure.prefixed(&step.name))?;
        }
        // And its AFTER step, over what the last function produced.
        match (&plan.code, &plan.templates) {
            (Some(code), _) => Ok(returned(
                code,
                RESPONSE_FUNCTION,
                &step_context(&base, &stash, &previous, Some(&previous)),
                &mut stash,
            )
            .await?),
            (None, Some(templates)) => match &templates.response {
                Some(template) => {
                    let context = step_context(&base, &stash, &previous, Some(&previous));
                    let rendered = self.rendered(template, &context, &mut stash).await?;
                    read_rendered(&rendered, RESPONSE_TEMPLATE)
                }
                None => Ok(previous),
            },
            (None, None) => Ok(previous),
        }
    }

    /// Run ONE step — a UNIT resolver, or one function of a pipeline — against its own data
    /// source, and answer what its definition made of the result.
    async fn run_step(
        &mut self,
        step: &PipelineStep,
        base: &FieldContext,
        stash: &mut Json,
        previous: &Json,
    ) -> Result<Json, Failure> {
        let Some(code) = &step.code else {
            // A definition written as VTL MAPPING TEMPLATES rather than as APPSYNC_JS
            // code, which is the other form `Resolver$requestMappingTemplate` /
            // `$responseMappingTemplate` carry; a step that carries neither has no
            // definition to run at all.
            return match &step.templates {
                Some(templates) => {
                    self.run_templates(step, templates, base, stash, previous)
                        .await
                }
                None => Err(NO_DEFINITION.to_string().into()),
            };
        };
        let recorded = self.recorded_step(&step.name);
        let requested = returned(
            code,
            REQUEST_FUNCTION,
            &step_context(base, stash, &Json::Null, Some(previous)),
            stash,
        )
        .await?;
        self.record_request(recorded, &requested);
        let result = self.data_source_result(step, &requested)?;
        let answered = returned(
            code,
            RESPONSE_FUNCTION,
            &step_context(base, stash, &result, Some(previous)),
            stash,
        )
        .await?;
        self.record_response(recorded, &answered);
        Ok(answered)
    }

    /// Run ONE step that is defined by MAPPING TEMPLATES: evaluate the request template to
    /// the request its data source is handed, resolve that request against the data source,
    /// and evaluate the response template over the result it produced.
    ///
    /// Both templates are evaluated over the field's own `$context` ([`step_context`]), and
    /// the `stash` each leaves behind travels on, as "a single stash object lives through a
    /// single resolver run". The request template renders the data source's request object,
    /// so its text is read as the JSON document that object is
    /// (`EvaluateMappingTemplate$evaluationResult`); the response template renders the value
    /// the field resolves to, so its text is read as that value. A template that is missing
    /// contributes nothing of its own: "VTL request mapping templates are optional when
    /// using an Lambda data source", and a missing response template leaves the data
    /// source's own result as the field's value.
    async fn run_templates(
        &mut self,
        step: &PipelineStep,
        templates: &Templates,
        base: &FieldContext,
        stash: &mut Json,
        previous: &Json,
    ) -> Result<Json, Failure> {
        let recorded = self.recorded_step(&step.name);
        let requested = match &templates.request {
            Some(template) => {
                let context = step_context(base, stash, &Json::Null, Some(previous));
                let rendered = self.rendered(template, &context, stash).await?;
                read_rendered(&rendered, REQUEST_TEMPLATE)?
            }
            None => Json::Null,
        };
        self.record_request(recorded, &requested);
        let result = self.data_source_result(step, &requested)?;
        let answered = match &templates.response {
            Some(template) => {
                let context = step_context(base, stash, &result, Some(previous));
                let rendered = self.rendered(template, &context, stash).await?;
                read_rendered(&rendered, RESPONSE_TEMPLATE)?
            }
            None => result,
        };
        self.record_response(recorded, &answered);
        Ok(answered)
    }

    /// Open this step's own entry in the field's trace, which the API's field logging reports
    /// as the request and the response the step GENERATED, and answer where that entry sits.
    /// Opened BEFORE the step's request half runs, so a step that raises is still reported as
    /// one that ran. `None` for an API that captures no field-level logs, which is what makes
    /// the collection cost nothing there.
    fn recorded_step(&mut self, name: &str) -> Option<usize> {
        if !self.traced || self.steps.len() >= MAX_TRACED_STEPS {
            return None;
        }
        self.steps.push(GeneratedStep {
            name: name.to_string(),
            request: None,
            response: None,
        });
        Some(self.steps.len().saturating_sub(1))
    }

    /// Report one error of this execution, under what one result may REPORT.
    ///
    /// The one place an error of a field is reported, so every kind of them — a bound a field
    /// met, a field the caller may not reach, what a definition raised and what it appended —
    /// is counted the same way. A result that would carry more than [`MAX_FIELD_ERRORS`]
    /// carries that many and ONE further entry saying so, because the number of values a
    /// resolver's answer projects a field over is the request's to choose and an error carries
    /// the path and the sentence of each.
    fn fault(&mut self, error: Json) {
        if self.errors.len() < MAX_FIELD_ERRORS {
            self.errors.push(error);
            return;
        }
        if self.errors.len() == MAX_FIELD_ERRORS {
            self.errors
                .push(json!({"message": format!("{TOO_MANY_ERRORS} {MAX_FIELD_ERRORS}.")}));
        }
    }

    /// Whether what one field's trace would RETAIN fits in what this request may retain,
    /// spending it if so.
    /// Weighed through the mapping-template runtime's own reading of what a value occupies
    /// (`mapping_template::weigh`), so one service weighs a held value one way; a value it
    /// refuses outright is one this request does not retain either.
    fn affordable(&mut self, context: &Json, steps: &[GeneratedStep]) -> bool {
        let mut weight = match mapping_template::weigh(context, 0) {
            Ok(weight) => weight,
            Err(_) => return false,
        };
        for step in steps {
            for generated in [step.request.as_ref(), step.response.as_ref()] {
                let Some(generated) = generated else {
                    continue;
                };
                match mapping_template::weigh(generated, 0) {
                    Ok(step_weight) => weight = weight.saturating_add(step_weight),
                    Err(_) => return false,
                }
            }
        }
        if weight > self.trace_bytes {
            return false;
        }
        self.trace_bytes -= weight;
        true
    }

    /// Record the request one step generated, where its entry was opened.
    fn record_request(&mut self, at: Option<usize>, requested: &Json) {
        if let Some(entry) = at.and_then(|at| self.steps.get_mut(at)) {
            entry.request = Some(requested.clone());
        }
    }

    /// Record what one step's response half answered, where its entry was opened.
    fn record_response(&mut self, at: Option<usize>, answered: &Json) {
        if let Some(entry) = at.and_then(|at| self.steps.get_mut(at)) {
            entry.response = Some(answered.clone());
        }
    }

    /// Evaluate one template over `context`, carrying onto `stash` what it left there and
    /// onto this execution the errors it appended.
    ///
    /// The evaluation is SYNCHRONOUS host work with no await point of its own, so it is run
    /// on the blocking pool rather than on the connection's own task — the same seam the
    /// APPSYNC_JS runs of this service use ([`appsync_js`]) — and the caller waits no longer
    /// than [`TEMPLATE_WAIT_CEILING`] for it. The budget it is given is what is LEFT of this
    /// request's own template time ([`MAX_TEMPLATE_RUNTIME`]), so the templates of one
    /// request cannot add up past it however many fields it selects.
    async fn rendered(
        &mut self,
        template: &str,
        context: &Json,
        stash: &mut Json,
    ) -> Result<String, Failure> {
        let budget = self
            .template_deadline
            .saturating_duration_since(::std::time::Instant::now());
        if budget.is_zero() {
            return Err(format!(
                "{TEMPLATES_TOOK_TOO_LONG} {} seconds.",
                MAX_TEMPLATE_RUNTIME.as_secs()
            )
            .into());
        }
        let owned = template.to_string();
        let over = context.clone();
        let evaluated = ::tokio::time::timeout(
            TEMPLATE_WAIT_CEILING,
            ::tokio::task::spawn_blocking(move || mapping_template::evaluate(&owned, &over, budget)),
        )
        .await;
        let evaluation = match evaluated {
            Ok(Ok(evaluation)) => evaluation?,
            // The run did not finish, or did not come back at all: the field is reported as
            // one that did not resolve rather than waited on.
            Ok(Err(_)) | Err(_) => return Err(TEMPLATE_DID_NOT_FINISH.to_string().into()),
        };
        if let Some(left) = evaluation.context.get(STASH_MEMBER) {
            *stash = left.clone();
        }
        self.appended.extend(evaluation.appended);
        Ok(evaluation.text)
    }

    /// The result the step's data source produces for the request its `request` function
    /// returned.
    ///
    /// `NONE` is "There is no data source", so the request's own `payload` IS the result —
    /// the local resolver AppSync documents. Every other type names a resource of another
    /// service, which this endpoint does not call, so the field is reported as one that did
    /// not resolve rather than answered with a value nothing produced.
    fn data_source_result(&self, step: &PipelineStep, requested: &Json) -> Result<Json, Failure> {
        let Some(data_source) = &step.data_source else {
            return Err(crate::state::DATA_SOURCE_NAME_MISSING_MESSAGE
                .to_string()
                .into());
        };
        if data_source.kind != LOCAL_DATA_SOURCE {
            return Err(format!(
                "{DATA_SOURCE_NOT_CALLED} {} data source {}.",
                data_source.kind, data_source.name
            )
            .into());
        }
        Ok(requested.get(PAYLOAD_MEMBER).cloned().unwrap_or(Json::Null))
    }

    /// The parts of a field's `ctx` that do not change between the steps that resolve it.
    fn field_context(&self, type_name: &str, source: &Json, field: &SelectedField) -> FieldContext {
        FieldContext {
            arguments: Json::Object(field.arguments.clone()),
            source: source.clone(),
            identity: self.request.identity.clone(),
            request: json!({"headers": Json::Object(self.request.headers.clone())}),
            info: json!({
                "fieldName": field.name,
                "parentTypeName": type_name,
                "variables": Json::Object(self.request.variables.clone()),
                "selectionSetList": field
                    .selections
                    .iter()
                    .map(|selected| json!(selected.name))
                    .collect::<Vec<Json>>(),
            }),
        }
    }
}

/// What resolves one field: the code, the data source and the pipeline the stored resolver
/// names, copied out of the store.
#[derive(Debug, Clone)]
struct ResolverPlan {
    /// The ARN of the stored resolver, which is what the field's own logs name it by.
    arn: Option<String>,
    /// The APPSYNC_JS `code` the resolver is defined by, or `None` for one defined another
    /// way.
    code: Option<String>,
    /// The mapping templates the resolver is defined by, which is the other way a
    /// definition is written.
    templates: Option<Templates>,
    /// The data source the resolver names, where the API still holds it.
    data_source: Option<StepDataSource>,
    /// The steps of a `PIPELINE` resolver, in the order the pipeline invokes them.
    functions: Vec<PipelineStep>,
    /// Whether the resolver is of the `PIPELINE` kind.
    is_pipeline: bool,
}

/// One step of a resolution: a UNIT resolver, or one function of a pipeline.
#[derive(Debug, Clone)]
struct PipelineStep {
    /// What the step is called, which is what an error of the step names it by.
    name: String,
    code: Option<String>,
    templates: Option<Templates>,
    data_source: Option<StepDataSource>,
}

/// The mapping templates one step is defined by: one per direction, either of which a
/// definition may leave out.
#[derive(Debug, Clone)]
struct Templates {
    request: Option<String>,
    response: Option<String>,
}

impl Templates {
    /// The templates a stored definition carries, or `None` for one that carries neither —
    /// which is a definition written another way.
    fn of(request: Option<&str>, response: Option<&str>) -> Option<Self> {
        (request.is_some() || response.is_some()).then(|| Self {
            request: request.map(str::to_string),
            response: response.map(str::to_string),
        })
    }
}

/// Why one step did not produce a value: a sentence about the step, or the error a mapping
/// template RAISED, which carries everything `$util.error` was handed.
#[derive(Debug, Clone)]
enum Failure {
    Message(String),
    /// What a mapping template RAISED, boxed: it carries everything `$util.error` was
    /// handed, which is far larger than the rest of this error channel.
    Raised(Box<Raised>),
}

impl Failure {
    /// This failure as the error of the field that did not resolve.
    fn document(&self, path: &[Json]) -> Json {
        match self {
            Self::Message(message) => field_error(path, message),
            Self::Raised(raised) => raised.document(path),
        }
    }

    /// This failure as one a NAMED step of a pipeline produced.
    fn prefixed(self, name: &str) -> Self {
        match self {
            Self::Message(message) => Self::Message(format!("{name}: {message}")),
            Self::Raised(mut raised) => {
                raised.message = format!("{name}: {}", raised.message);
                Self::Raised(raised)
            }
        }
    }

    /// The sentence this failure states, which is what the API's field logging reports as
    /// the error of the field that did not resolve.
    fn message(&self) -> String {
        match self {
            Self::Message(message) => message.clone(),
            Self::Raised(raised) => raised.message.clone(),
        }
    }
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

impl From<mapping_template::Fault> for Failure {
    fn from(fault: mapping_template::Fault) -> Self {
        match fault {
            mapping_template::Fault::Raised(raised) => Self::Raised(raised),
            mapping_template::Fault::Fault(reason) => Self::Message(reason),
        }
    }
}

/// The text a rendered template produced, as the value it stands for: a template renders
/// the JSON of that value, and one that rendered nothing stands for nothing.
fn read_rendered(rendered: &str, direction: &str) -> Result<Json, Failure> {
    let text = rendered.trim();
    if text.is_empty() {
        return Ok(Json::Null);
    }
    ::serde_json::from_str(text)
        .map_err(|error| Failure::Message(format!("{direction} {NOT_RENDERED_JSON} {error}")))
}

/// The data source one step resolves against: its type, and the name it is held under.
#[derive(Debug, Clone)]
struct StepDataSource {
    kind: String,
    name: String,
}

/// The data source `name` of `api`, or `None` when the API holds none under that name.
fn step_data_source(api: &GraphqlApiRecord, name: Option<&str>) -> Option<StepDataSource> {
    let name = name?;
    api.data_sources
        .get(name)
        .map(|record: &DataSourceRecord| StepDataSource {
            kind: record.r#type.clone(),
            name: record.name.clone(),
        })
}

/// The steps a `PIPELINE` resolver invokes, in the order its `pipelineConfig` names them.
///
/// A function the API no longer holds contributes a step with no definition, which is
/// reported as a step that could not run rather than skipped: the pipeline names it.
fn pipeline_steps(api: &GraphqlApiRecord, resolver: &ResolverRecord) -> Vec<PipelineStep> {
    resolver
        .pipeline_config
        .as_ref()
        .and_then(|pipeline| pipeline.functions.clone())
        .unwrap_or_default()
        .iter()
        .map(|function_id| match api.functions.get(function_id) {
            Some(function) => PipelineStep {
                name: function.name.clone(),
                code: function.code.clone(),
                templates: Templates::of(
                    function.request_mapping_template.as_deref(),
                    function.response_mapping_template.as_deref(),
                ),
                data_source: step_data_source(api, Some(&function.data_source_name)),
            },
            None => PipelineStep {
                name: function_id.clone(),
                code: None,
                templates: None,
                data_source: None,
            },
        })
        .collect()
}

/// The parts of a field's `ctx` that every step resolving it shares.
#[derive(Debug, Clone)]
struct FieldContext {
    arguments: Json,
    source: Json,
    request: Json,
    info: Json,
    identity: Json,
}

/// The CONTEXT a verbose field log carries for one field: the arguments the selection carries
/// and which field of which type is being resolved.
///
/// Deliberately NOT the `ctx` a resolver reads. What a log record keeps is read by whoever can
/// read the log group and is held for the whole request, so it carries neither the request's
/// own fields (where a caller presents its authorization material) nor the request's variables
/// or the parent's resolved value (which one request would otherwise retain once per field it
/// resolves).
fn logged_context(type_name: &str, field: &SelectedField) -> Json {
    json!({
        "arguments": Json::Object(field.arguments.clone()),
        "info": {
            "fieldName": field.name,
            "parentTypeName": type_name,
        },
    })
}

/// The `ctx` one step is run with: what the field carries, the stash as the steps before it
/// left it, the result the data source produced and the previous step's result.
///
/// The member names are the ones APPSYNC_JS code reads them under: `arguments` (with `args`
/// as its documented alias), `source`, `identity`, `request`, `info`, `stash`, `result` and
/// `prev`.
fn step_context(
    base: &FieldContext,
    stash: &Json,
    result: &Json,
    previous: Option<&Json>,
) -> Json {
    json!({
        "arguments": base.arguments,
        "args": base.arguments,
        "source": base.source,
        // Who the request presented itself as, which the endpoint resolved before anything
        // ran (`super::query`): the principal of the call for an authorization mode that
        // names one, and `null` for one that names none — an API key names no principal.
        "identity": base.identity,
        "request": base.request,
        "info": base.info,
        "stash": stash,
        "result": result,
        "prev": json!({"result": previous.cloned().unwrap_or(Json::Null)}),
        "error": Json::Null,
    })
}

/// Run `function` of `code` over `context` and answer what it returned, carrying the stash
/// it left behind onto `stash`.
///
/// `Err` is a step that did not return: the code raised (`util.error` is how a resolver
/// refuses), it defines no such function, or one of the engine's bounds stopped it. The text
/// is the engine's own.
async fn returned(
    code: &str,
    function: &str,
    context: &Json,
    stash: &mut Json,
) -> Result<Json, String> {
    let evaluation =
        appsync_js::evaluate(code.to_string(), function.to_string(), context.clone()).await;
    if let Some(left) = evaluation
        .stash
        .as_deref()
        .and_then(|text| ::serde_json::from_str::<Json>(text).ok())
    {
        *stash = left;
    }
    match evaluation.outcome {
        Outcome::Returned(text) => Ok(::serde_json::from_str(&text).unwrap_or(Json::Null)),
        Outcome::Raised(message) | Outcome::Stopped(message) => Err(message),
        Outcome::Faults(faults) => Err(faults
            .first()
            .map(|fault| fault.value.clone())
            .unwrap_or_else(|| NO_DEFINITION.to_string())),
    }
}

/// One error of a field that did not resolve, as a GraphQL result reports one: where in the
/// result the field sits, and what went wrong.
fn field_error(path: &[Json], message: &str) -> Json {
    json!({"path": path.to_vec(), "message": message})
}

/// The error AppSync reports for a field the caller may not access: its own path, the
/// `Unauthorized` error type and the sentence naming the field and the type it belongs to.
fn unauthorized_field(path: &[Json], type_name: &str, field_name: &str) -> Json {
    json!({
        "path": path.to_vec(),
        "errorType": UNAUTHORIZED_FIELD_ERROR_TYPE,
        "message": format!("{UNAUTHORIZED_FIELD} {field_name} on type {type_name}"),
    })
}

/// The `errorType` and the fixed part of the sentence that error states.
const UNAUTHORIZED_FIELD_ERROR_TYPE: &str = "Unauthorized";
const UNAUTHORIZED_FIELD: &str = "Not Authorized to access";

/// The named type a reference resolves to, whatever it is wrapped in: a list of non-null
/// `Post`s is still a reference to `Post`.
fn named_type(reference: &crate::state::TypeRef) -> String {
    match reference {
        crate::state::TypeRef::Named(name) => name.clone(),
        crate::state::TypeRef::List(inner) | crate::state::TypeRef::NonNull(inner) => {
            named_type(inner)
        }
    }
}

/// The scalar AppSync writes a value out as a JSON string: `AWSJSON` is "a JSON string",
/// where every other scalar is written as the value itself.
const JSON_SCALAR: &str = "AWSJSON";

/// `value` as the type `reference` declares writes it out.
///
/// The one coercion a resolved value needs: a value of the `AWSJSON` scalar is written as the
/// JSON TEXT of it — "a JSON string. Any valid JSON construct" — so a client reads the value
/// back by parsing that string, whatever construct the resolver produced. A list of them
/// coerces element by element, because each element is one such value, and `null` stays
/// `null`: no value was produced to write out.
fn json_coerced(reference: &crate::state::TypeRef, value: Json) -> Json {
    match reference {
        crate::state::TypeRef::NonNull(inner) => json_coerced(inner, value),
        crate::state::TypeRef::List(inner) => match value {
            Json::Array(elements) => Json::Array(
                elements
                    .into_iter()
                    .map(|element| json_coerced(inner, element))
                    .collect(),
            ),
            other => other,
        },
        crate::state::TypeRef::Named(name) if name == JSON_SCALAR => match value {
            Json::Null => Json::Null,
            other => Json::String(other.to_string()),
        },
        crate::state::TypeRef::Named(_) => value,
    }
}

/// Which root operation type an operation's fields belong to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind {
    Query,
    Mutation,
    Subscription,
}

/// One operation of a document: which kind it is, and the fields it selects.
struct Operation {
    kind: OperationKind,
    selection_set: Option<::apollo_parser::cst::SelectionSet>,
}

/// The operation to execute: the one the request named, or — for a request that names none —
/// the document's only operation, which is the only case GraphQL leaves unambiguous
/// ("if `operationName` is null ... the document is expected to contain only one
/// operation").
fn selected_operation(
    document: &::apollo_parser::cst::Document,
    operation_name: Option<&str>,
) -> Option<Operation> {
    use ::apollo_parser::cst;
    let mut operations: Vec<cst::OperationDefinition> = document
        .definitions()
        .filter_map(|definition| match definition {
            cst::Definition::OperationDefinition(operation) => Some(operation),
            _ => None,
        })
        .collect();
    let chosen = match operation_name {
        Some(wanted) => operations.into_iter().find(|operation| {
            operation
                .name()
                .is_some_and(|name| name.text().as_str() == wanted)
        })?,
        None if operations.len() == 1 => operations.remove(0),
        None => return None,
    };
    let kind = match chosen.operation_type() {
        None => OperationKind::Query,
        Some(declared) => {
            if declared.mutation_token().is_some() {
                OperationKind::Mutation
            } else if declared.subscription_token().is_some() {
                OperationKind::Subscription
            } else {
                OperationKind::Query
            }
        }
    };
    Some(Operation {
        kind,
        selection_set: chosen.selection_set(),
    })
}

/// One selected field of a selection set: the key it is reported under, the field it
/// selects, the arguments it carries and its own sub-selection.
#[derive(Debug, Clone)]
pub struct SelectedField {
    /// The response key: the alias where the selection carries one, and the field's own name
    /// otherwise ("the response key of a selection is its alias if one is defined").
    pub key: String,
    pub name: String,
    pub arguments: JsonMap<String, Json>,
    pub selections: Vec<SelectedField>,
}

/// Reading ONE request's document into the fields it selects: what the reading needs, and
/// what bounds how much of it the document may ask for.
///
/// The budget is what makes the expansion safe. A selection set's own width
/// ([`MAX_SELECTED_FIELDS`]) and the depth it may nest to ([`MAX_SELECTION_DEPTH`]) bound
/// each LEVEL, and their product does not bound the total — a chain of fragments each
/// spreading the next multiplies one level's width by the next's — so the number of NODES the
/// whole document expands to is counted down from [`MAX_SELECTED_NODES`] across every level
/// and every splice: every field read and every fragment spliced spends one, so a document
/// whose fragments select no field at all is bounded too. A document that asks for more is
/// refused before the expansion is built, so one request cannot choose how much work or
/// memory reading it costs.
struct Expansion<'a> {
    document: &'a ::apollo_parser::cst::Document,
    variables: &'a JsonMap<String, Json>,
    /// How many more fields this document may expand to, all levels together.
    remaining: usize,
    /// How many more bytes of substituted ARGUMENT this document may hold, which
    /// [`MAX_ARGUMENT_BYTES`] starts at ([`read_arguments`] states why it is counted).
    held: usize,
    /// The fragments being spliced right now, innermost last: a spread that re-enters one is
    /// refused rather than expanded, so a cyclic fragment is reported instead of fanning
    /// out until a bound stops it.
    spreading: Vec<String>,
}

impl Expansion<'_> {
    /// Read one selection set into the fields it selects, splicing in the selections of
    /// every fragment it spreads.
    fn selection_set(
        &mut self,
        selection_set: Option<&::apollo_parser::cst::SelectionSet>,
        depth: usize,
    ) -> Result<Vec<SelectedField>, String> {
        use ::apollo_parser::cst;
        if depth >= MAX_SELECTION_DEPTH {
            return Err(format!("{TOO_DEEP} {MAX_SELECTION_DEPTH}."));
        }
        let Some(selection_set) = selection_set else {
            return Ok(Vec::new());
        };
        let mut fields: Vec<SelectedField> = Vec::new();
        for selection in selection_set.selections() {
            if fields.len() >= MAX_SELECTED_FIELDS {
                return Err(format!("{TOO_MANY_FIELDS} {MAX_SELECTED_FIELDS}."));
            }
            match selection {
                cst::Selection::Field(selected) => {
                    let Some(name) = selected.name().map(|name| name.text().to_string()) else {
                        continue;
                    };
                    let key = selected
                        .alias()
                        .and_then(|alias| alias.name())
                        .map(|alias| alias.text().to_string())
                        .unwrap_or_else(|| name.clone());
                    // Spent BEFORE the field is read, so the sub-selection below is read
                    // under what is left rather than past it.
                    self.spend()?;
                    let arguments =
                        read_arguments(selected.arguments(), self.variables, &mut self.held)?;
                    let selections =
                        self.selection_set(selected.selection_set().as_ref(), depth + 1)?;
                    fields.push(SelectedField {
                        key,
                        name,
                        arguments,
                        selections,
                    });
                }
                // An inline fragment selects on the same object its parent does, so its own
                // selections are the parent's. The type condition decides nothing here: what
                // a field resolves to is the resolver attached to it, not the document's
                // guess at the object's type.
                cst::Selection::InlineFragment(inline) => {
                    // Spent for the SPLICE as well as for the fields it produces: a spread
                    // that selects no field at all is still work this document asked for, so
                    // it is counted against the same budget.
                    self.spend()?;
                    let spliced = self.selection_set(inline.selection_set().as_ref(), depth + 1)?;
                    fields.extend(spliced);
                    self.within_width(fields.len())?;
                }
                cst::Selection::FragmentSpread(spread) => {
                    self.spend()?;
                    let Some(name) = spread
                        .fragment_name()
                        .and_then(|fragment| fragment.name())
                        .map(|name| name.text().to_string())
                    else {
                        continue;
                    };
                    if self.spreading.contains(&name) {
                        return Err(format!("{CYCLIC_FRAGMENT} {name}."));
                    }
                    let Some(definition) = fragment_definition(self.document, &name) else {
                        return Err(format!("{NO_SUCH_FRAGMENT} {name}."));
                    };
                    self.spreading.push(name);
                    let spliced = self.selection_set(Some(&definition), depth + 1);
                    self.spreading.pop();
                    fields.extend(spliced?);
                    self.within_width(fields.len())?;
                }
            }
        }
        Ok(fields)
    }

    /// Spend one node of this document's budget, or refuse the document for asking for more
    /// than [`MAX_SELECTED_NODES`] nodes in all.
    fn spend(&mut self) -> Result<(), String> {
        if self.remaining == 0 {
            return Err(format!("{TOO_MANY_NODES} {MAX_SELECTED_NODES}."));
        }
        self.remaining -= 1;
        Ok(())
    }

    /// Refuse a selection set that `selected` fields wide: the width is checked again after
    /// a splice, because a fragment's own selections land in the set all at once and so can
    /// carry it past [`MAX_SELECTED_FIELDS`] rather than up to it.
    fn within_width(&self, selected: usize) -> Result<(), String> {
        if selected > MAX_SELECTED_FIELDS {
            return Err(format!("{TOO_MANY_FIELDS} {MAX_SELECTED_FIELDS}."));
        }
        Ok(())
    }
}

/// The selection set the fragment `name` defines, or `None` when the document defines no
/// such fragment.
fn fragment_definition(
    document: &::apollo_parser::cst::Document,
    name: &str,
) -> Option<::apollo_parser::cst::SelectionSet> {
    use ::apollo_parser::cst;
    document.definitions().find_map(|definition| {
        let cst::Definition::FragmentDefinition(fragment) = definition else {
            return None;
        };
        let defined = fragment
            .fragment_name()
            .and_then(|fragment| fragment.name())
            .map(|name| name.text().to_string())?;
        (defined == name).then(|| fragment.selection_set())?
    })
}

/// The arguments one selected field carries, as the document writes them and with the
/// request's own variables substituted for every variable it references.
///
/// `held` is what the whole document may hold in its substituted arguments, counted down
/// across every field it selects: a variable's value is substituted by COPY into each
/// argument that references it, so what the expansion retains is not bounded by the size of
/// the document. `Err` once that is spent, so a document that would expand its own body into
/// more than this is refused before anything is resolved.
fn read_arguments(
    arguments: Option<::apollo_parser::cst::Arguments>,
    variables: &JsonMap<String, Json>,
    held: &mut usize,
) -> Result<JsonMap<String, Json>, String> {
    let mut read = JsonMap::new();
    let Some(arguments) = arguments else {
        return Ok(read);
    };
    for argument in arguments.arguments() {
        let Some(name) = argument.name().map(|name| name.text().to_string()) else {
            continue;
        };
        let Some(value) = argument.value() else {
            continue;
        };
        read.insert(name, read_value(&value, variables, 0, held)?);
    }
    Ok(read)
}

/// One GraphQL value as the document writes it, in the JSON a resolver reads it as.
///
/// A variable takes the value the request's `variables` gives it, and one the request does
/// not carry is `null` — "if a variable is not provided, it is considered to have no value".
/// Every value this produces is charged against `held` ([`read_arguments`] states why),
/// weighed through the mapping-template runtime's own reading of what a value occupies, so
/// one service weighs a held value one way.
fn read_value(
    value: &::apollo_parser::cst::Value,
    variables: &JsonMap<String, Json>,
    depth: usize,
    held: &mut usize,
) -> Result<Json, String> {
    use ::apollo_parser::cst::{CstNode, Value};
    if depth >= MAX_SELECTION_DEPTH {
        return Ok(Json::Null);
    }
    let read = match value {
        Value::Variable(variable) => variable
            .name()
            .and_then(|name| variables.get(name.text().as_str()).cloned())
            .unwrap_or(Json::Null),
        Value::StringValue(text) => Json::String(String::from(text.clone())),
        Value::ListValue(list) => {
            let mut elements: Vec<Json> = Vec::new();
            for element in list.values() {
                elements.push(read_value(
                    &element,
                    variables,
                    depth.saturating_add(1),
                    held,
                )?);
            }
            return Ok(Json::Array(elements));
        }
        Value::ObjectValue(object) => {
            let mut members = JsonMap::new();
            for member in object.object_fields() {
                let (Some(name), Some(value)) = (member.name(), member.value()) else {
                    continue;
                };
                members.insert(
                    name.text().to_string(),
                    read_value(&value, variables, depth.saturating_add(1), held)?,
                );
            }
            return Ok(Json::Object(members));
        }
        // An enum value is a name, which is how a resolver reads one; every remaining form
        // is a JSON literal already — an integer, a float, a boolean or `null` — so it is
        // read as the document wrote it.
        Value::EnumValue(enumerated) => enumerated
            .name()
            .map(|name| Json::String(name.text().to_string()))
            .unwrap_or(Json::Null),
        other => {
            let text = other.syntax().text().to_string();
            ::serde_json::from_str(text.trim()).unwrap_or(Json::Null)
        }
    };
    // A list and an object are charged through their own elements above, so only the values
    // that are read whole are weighed here — which is where a substituted variable lands.
    let weight = mapping_template::weigh(&read, 0)
        .map_err(|_| format!("{TOO_MUCH_ARGUMENT} {MAX_ARGUMENT_BYTES} bytes."))?;
    if weight > *held {
        return Err(format!("{TOO_MUCH_ARGUMENT} {MAX_ARGUMENT_BYTES} bytes."));
    }
    *held -= weight;
    Ok(read)
}

/// How an operation the document does not hold is stated.
fn no_such_operation(operation_name: Option<&str>) -> String {
    match operation_name {
        Some(named) => format!("{NO_SUCH_OPERATION} {named}."),
        None => ONE_OPERATION.to_string(),
    }
}

/// The sentences this module's own refusals state.
const NOT_A_DOCUMENT: &str = "The request's query is not a GraphQL document:";
const NO_SUCH_OPERATION: &str = "This document declares no operation named";
const ONE_OPERATION: &str =
    "A request that names no operation must carry a document with exactly one operation.";
const SUBSCRIPTIONS_ARE_REALTIME: &str =
    "A subscription is served at this API's real-time endpoint.";
const NO_SUCH_FRAGMENT: &str = "This document spreads a fragment it does not define:";
const TOO_DEEP: &str = "This endpoint resolves a selection set no deeper than";
const TOO_MANY_FIELDS: &str = "A selection set carries at most";
const TOO_MANY_NODES: &str = "One request selects at most";
const CYCLIC_FRAGMENT: &str = "This document spreads a fragment into itself:";
const TOO_MANY_RUNS: &str = "One request runs at most";
const TOO_MANY_ERRORS: &str = "This request's result reports at most";
const TOO_MUCH_ARGUMENT: &str = "One request's selected arguments hold at most";
const NO_DEFINITION: &str = "This field's resolver carries no definition to run.";
const TEMPLATES_TOOK_TOO_LONG: &str =
    "This request's mapping templates ran for longer than this endpoint evaluates them in:";
const TEMPLATE_DID_NOT_FINISH: &str =
    "This field's mapping template did not finish, so the field did not resolve.";
const NOT_RENDERED_JSON: &str = "mapping template did not render a JSON value:";
const REQUEST_TEMPLATE: &str = "The request";
const RESPONSE_TEMPLATE: &str = "The response";

/// The member of a resolver's `$context` the stash is carried in.
const STASH_MEMBER: &str = "stash";
const DATA_SOURCE_NOT_CALLED: &str = "This endpoint does not call the";
const NO_PIPELINE_FUNCTIONS: &str = "This field's pipeline resolver invokes no function.";
