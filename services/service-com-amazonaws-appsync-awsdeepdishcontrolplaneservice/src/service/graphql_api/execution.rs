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

use ::std::collections::BTreeMap;
use ::std::time::Duration;

use ::serde_json::{json, Map as JsonMap, Value as Json};

use super::lambda;
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

/// How many fields one request may run a resolver for, and how many ENGINE RUNS those fields
/// may make between them.
///
/// Emulator resource guards, NOT AWS quotas: each run is a fresh engine with its own bounds,
/// and one field's resolver expands into a run per step of it — the before step, each function
/// of a pipeline and the after step — so the number of runs a request can ask for is the
/// PRODUCT of the fields it selects and the steps each one carries. Both factors are counted
/// down here rather than either alone, and a field that meets the second bound is reported as
/// the field that met it rather than being run.
const MAX_RESOLVER_RUNS: usize = 64;
const MAX_ENGINE_RUNS: usize = 256;

/// How long ALL the APPSYNC_JS evaluations of ONE request may be STARTED over, between them.
///
/// The same guard the mapping templates of one request share ([`MAX_TEMPLATE_RUNTIME`]), for
/// the same reason: every evaluation is handed to a blocking worker with a budget of its own,
/// so what one request can occupy is bounded by this span rather than by the sum of the
/// budgets of the runs it asks for. A field whose step would start past it is reported as the
/// field that met the bound.
const MAX_ENGINE_RUNTIME: ::std::time::Duration = ::std::time::Duration::from_secs(10);

/// How many steps of a `PIPELINE` resolver one field may run: "Functions per pipeline
/// resolver" is 10 in AWS AppSync's own service quotas, so a stored resolver naming more
/// steps than this is one no AppSync pipeline has, and the field is reported as the field that
/// met the bound rather than run with the steps that fit.
const MAX_PIPELINE_STEPS: usize = 10;

/// How many sibling resolutions ONE invocation of a batched resolver carries where the
/// resolver configures no maximum batching size of its own: as many as one may configure
/// ("maxBatchSize", whose range is 0 to 2,000), which is also what bounds the gathering of the
/// resolutions a definition's own `BatchInvoke` merges.
const MAX_BATCHED_RESOLUTIONS: usize = 2_000;

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

/// How many bytes of INTROSPECTION answer one request may be given, every introspection
/// field of it together.
///
/// An emulator resource guard of the same kind as [`MAX_ARGUMENT_BYTES`], and the one that
/// bounds the AMPLIFICATION rather than one answer: `__schema` answers the API's whole type
/// system, whose size is the schema the owner installed, and how many introspection fields a
/// document selects is the REQUEST's to choose ([`MAX_SELECTED_FIELDS`] per level,
/// [`MAX_SELECTED_NODES`] in all) — so the PRODUCT of the two is counted down from here
/// rather than either factor alone. A field whose answer no longer fits resolves to `null`
/// and is reported as the field that met the bound.
const MAX_INTROSPECTED_BYTES: usize = 4 * 1024 * 1024;

/// How many bytes of ANSWER one request may be built, every field of every level together.
///
/// An emulator resource guard of the same kind as the three above, and the one that bounds
/// what the result HOLDS: a response key is the selection's own alias, so one field may be
/// selected many times in a selection set, and each selection is answered by a value of its
/// own — a member of the value its parent resolved to, an element of a list, or a projection
/// of one — so the bytes a result accumulates are the request's to choose along both the
/// width and the depth of its selection tree. Every value a field resolves to is counted down
/// from here as it is placed in the result ([`Execution::affordable_answer`]), the projected
/// values of an introspection answer among them, and the field that exhausts it resolves to
/// `null` and is reported as the field that met the bound.
const MAX_ANSWER_BYTES: usize = 32 * 1024 * 1024;

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

/// The `DataSourceType` of a data source whose resource is an EVENT BUS: a step resolving
/// against one publishes the events its request half returned onto that bus
/// ([`super::event_bridge`]).
const EVENT_BRIDGE_DATA_SOURCE: &str = "AMAZON_EVENTBRIDGE";

/// The `DataSourceType` of a data source that IS a function of AWS Lambda: "the data source
/// is an Lambda function" (`DataSource$type`), which a resolver bound to it resolves against
/// by INVOKING ([`super::lambda`]).
const LAMBDA_DATA_SOURCE: &str = "AWS_LAMBDA";

/// The member a request function returns the result of a local resolver in, and — for a data
/// source that is a function — the event that function is invoked with: AppSync's Lambda
/// data-source request carries `{"version", "operation", "payload"}`, and the `payload` IS
/// the event.
const PAYLOAD_MEMBER: &str = "payload";

/// The member that request names the INVOCATION by, and the value that names the batched one:
/// `BatchInvoke` is the operation whose event is the LIST of the events of the field
/// resolutions merged into it, and `Invoke` (the other value) the one whose event is the one
/// field's own.
const OPERATION_MEMBER: &str = "operation";
const BATCH_INVOKE_OPERATION: &str = "BatchInvoke";

/// The members one element of a BATCHED answer reports an error of ITS OWN item with: the
/// sentence that item failed with, and the class of it. An element carrying a sentence is the
/// one sibling that did not resolve; the others are unaffected.
const ITEM_ERROR_MESSAGE_MEMBER: &str = "errorMessage";
const ITEM_ERROR_TYPE_MEMBER: &str = "errorType";

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
    // operation's root, which is what says WHICH resolvers the selection addresses — beside
    // the QUERY root, which is the type the introspection system's own two root fields are
    // fields of ([`Execution::introspected`]).
    let Some((root_type, query_root)) = context
        .state
        .lock()
        .graphql_apis
        .get(&context.api_id)
        .map(|api| {
            let root_type = match kind {
                OperationKind::Query => api.query_root_type(),
                OperationKind::Mutation => api.mutation_root_type(),
                OperationKind::Subscription => api.subscription_root_type(),
            };
            (root_type, api.query_root_type())
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
        query_root,
        introspected: None,
        introspected_bytes: MAX_INTROSPECTED_BYTES,
        answer_bytes: MAX_ANSWER_BYTES,
        runs: 0,
        engine_runs: 0,
        errors: Vec::new(),
        started: ::std::time::Instant::now(),
        traced: super::field_logs::captures_field_logs(&context.state.lock(), &context.api_id),
        traces: Vec::new(),
        trace_bytes: MAX_TRACE_BYTES,
        steps: Vec::new(),
        appended: Vec::new(),
        template_deadline: ::std::time::Instant::now() + MAX_TEMPLATE_RUNTIME,
        engine_deadline: ::std::time::Instant::now() + MAX_ENGINE_RUNTIME,
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
    /// The type the API's schema binds as its QUERY root, which is the type the introspection
    /// system's own `__schema` and `__type` are fields of.
    query_root: String,
    /// The API's type system as introspection reports it, rendered at most ONCE per request
    /// ([`Execution::introspection_document`]) — `None` until a selected field needs it.
    introspected: Option<Json>,
    /// How many more bytes of INTROSPECTION answer this request may be given, which
    /// [`MAX_INTROSPECTED_BYTES`] starts at.
    introspected_bytes: usize,
    /// How many more bytes of ANSWER it may be built, which [`MAX_ANSWER_BYTES`] starts at.
    answer_bytes: usize,
    /// How many resolvers this request has run, which [`MAX_RESOLVER_RUNS`] bounds.
    runs: usize,
    /// How many ENGINE RUNS its fields have made between them, which [`MAX_ENGINE_RUNS`]
    /// bounds.
    engine_runs: usize,
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
    /// The same for its APPSYNC_JS evaluations: no step of a resolver is STARTED past it
    /// ([`MAX_ENGINE_RUNTIME`]).
    engine_deadline: ::std::time::Instant,
}

impl Execution<'_> {
    /// Resolve every field of one selection set against `source`, the value the parent field
    /// resolved to, and answer the object a GraphQL result carries them in.
    ///
    /// The selections are walked in the order the document wrote them, except that the SIBLING
    /// resolutions of one batched resolver are performed TOGETHER: a resolver that configures a
    /// maximum batching size resolves the siblings that address it in ONE invocation of its
    /// data source ([`Self::batched_results`]), so each of them takes its value from that one
    /// answer rather than from an invocation of its own.
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
            let parents = vec![(path, source.clone())];
            self.resolve_fields_over(type_name, parents, fields, depth)
                .await
                .into_iter()
                .next()
                .unwrap_or_else(|| Json::Object(JsonMap::new()))
        })
    }

    /// Resolve `fields` over EVERY parent in `parents` — the value each resolved to and where in
    /// the result it sits — and answer the object a GraphQL result carries each parent's
    /// selection in, in the parents' own order.
    ///
    /// One call per LEVEL of the result rather than per parent, because that is the span a
    /// batched resolver batches over: the resolutions of one batched field across SIBLING
    /// parents (the elements one list resolved to) are merged into one invocation of its data
    /// source just as the resolutions of its siblings under one parent are
    /// ([`Self::batch_groups`]), so each of them takes its value from that one answer.
    fn resolve_fields_over<'f>(
        &'f mut self,
        type_name: &'f str,
        parents: Vec<(Vec<Json>, Json)>,
        fields: &'f [SelectedField],
        depth: usize,
    ) -> ::std::pin::Pin<Box<dyn ::std::future::Future<Output = Vec<Json>> + Send + 'f>> {
        Box::pin(async move {
            // The resolutions ONE invocation of a batched resolver carries are performed
            // BEFORE the walk, so each member of such a group takes its value from that one
            // answer rather than from an invocation of its own. Only the resolutions a group
            // GATHERED are held, keyed by where each sits — that collection is as large as the
            // gathering, which [`Self::batch_groups`] bounds.
            let mut ready: BTreeMap<(usize, usize), Result<Json, Failure>> = BTreeMap::new();
            for (plan, members) in self.batch_groups(type_name, &parents, fields) {
                let results = self
                    .batched_results(&plan, type_name, &parents, fields, &members)
                    .await;
                for (member, result) in members.iter().zip(results) {
                    ready.insert((member.parent, member.field), result);
                }
            }
            let mut answers = Vec::with_capacity(parents.len());
            for (index, (path, source)) in parents.iter().enumerate() {
                let mut resolved = JsonMap::new();
                for (at, field) in fields.iter().enumerate() {
                    let mut field_path = path.clone();
                    field_path.push(json!(field.key));
                    let prepared = ready.remove(&(index, at));
                    let value = self
                        .resolve_field(type_name, source, field, &field_path, depth, prepared)
                        .await;
                    // What the RESULT holds is counted down in host code, for the reason
                    // [`MAX_ANSWER_BYTES`] records: every value placed in it passes here.
                    let value = self.affordable_answer(value, &field_path);
                    resolved.insert(field.key.clone(), value);
                }
                answers.push(Json::Object(resolved));
            }
            answers
        })
    }

    /// Resolve ONE field: run the resolver attached to it where the API holds one, take the
    /// member of the parent object where it does not, and project the field's own
    /// sub-selection onto whatever that came to.
    ///
    /// `ready` is the value a resolution PERFORMED ALREADY came to — what one batched
    /// invocation produced for this field ([`Self::batched_results`]) — so the field is traced,
    /// reported and projected here exactly as one resolved on its own, and nothing of it is run
    /// a second time.
    async fn resolve_field(
        &mut self,
        type_name: &str,
        source: &Json,
        field: &SelectedField,
        path: &[Json],
        depth: usize,
        ready: Option<Result<Json, Failure>>,
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
        // The fields GraphQL's INTROSPECTION SYSTEM adds to every schema are answered from
        // the API's own type system, before any resolver is looked for: no resolver can be
        // attached to one, because each belongs to no type the schema declares
        // ([`super::introspection`]).
        if let Some((introspected_type, value)) = self.introspected(type_name, field) {
            // What the introspection answers of ONE request may hold is counted down in host
            // code, for the reason [`MAX_INTROSPECTED_BYTES`] records.
            if !self.affordable_introspection(&value) {
                self.fault(field_error(
                    path,
                    &format!("{TOO_MUCH_INTROSPECTION} {MAX_INTROSPECTED_BYTES} bytes."),
                ));
                return Json::Null;
            }
            if field.selections.is_empty() || value.is_null() {
                return value;
            }
            return self
                .projected(introspected_type, value, field, path, depth)
                .await;
        }
        let plan = self.plan(type_name, &field.name);
        let resolver_arn = plan.as_ref().and_then(|plan| plan.arn.clone());
        // Where this field sits in the execution, so its trace reports the offset and the
        // span the API's field logging states them as.
        let start_offset = self.started.elapsed();
        self.steps.clear();
        self.appended.clear();
        let resolved: Result<Json, Failure> = match ready {
            // A resolution this field's own siblings performed WITH it: the value is already
            // what the resolver came to, so nothing of it is run again.
            Some(ready) => ready,
            None => match plan {
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
            },
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
                    // The elements that are OBJECTS have the selection projected onto them
                    // TOGETHER, because they are the sibling parents of one level of the
                    // result: a batched resolver merges the resolutions of one field across
                    // them into one invocation of its data source
                    // ([`Self::resolve_fields_over`]). Everything else is projected on its
                    // own, as a nested list is a level of its own and a scalar carries no
                    // sub-selection.
                    let inner = depth.saturating_add(1);
                    let mut projected: Vec<Option<Json>> = Vec::with_capacity(elements.len());
                    let mut parents: Vec<(Vec<Json>, Json)> = Vec::new();
                    let mut filled: Vec<usize> = Vec::new();
                    for (index, element) in elements.into_iter().enumerate() {
                        let mut element_path = path.to_vec();
                        element_path.push(json!(index));
                        if matches!(element, Json::Object(_)) {
                            if inner >= MAX_SELECTION_DEPTH {
                                self.fault(field_error(
                                    &element_path,
                                    &format!("{TOO_DEEP} {MAX_SELECTION_DEPTH}."),
                                ));
                                projected.push(Some(Json::Null));
                                continue;
                            }
                            filled.push(index);
                            parents.push((element_path, element));
                            projected.push(None);
                            continue;
                        }
                        let resolved = self
                            .projected(child_type, element, field, &element_path, inner)
                            .await;
                        projected.push(Some(resolved));
                    }
                    if !parents.is_empty() {
                        let resolved = self
                            .resolve_fields_over(
                                child_type,
                                parents,
                                &field.selections,
                                inner.saturating_add(1),
                            )
                            .await;
                        for (at, value) in filled.into_iter().zip(resolved) {
                            if let Some(slot) = projected.get_mut(at) {
                                *slot = Some(value);
                            }
                        }
                    }
                    Json::Array(
                        projected
                            .into_iter()
                            .map(|slot| slot.unwrap_or(Json::Null))
                            .collect(),
                    )
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
            max_batch_size: resolver.max_batch_size,
        })
    }

    /// What one of GraphQL's introspection fields answers for this field, as `(the
    /// introspection type the value is of, the value)`, or `None` for a field that is none
    /// of them.
    ///
    /// `__typename` is available on every object and names the type the selection is on;
    /// `__schema` and `__type(name:)` are fields of the QUERY ROOT and report the type system
    /// itself, so they are answered only where the selection is on that root. All three are
    /// read off the one type-system snapshot this request renders
    /// ([`Self::introspection_document`]).
    fn introspected(
        &mut self,
        type_name: &str,
        field: &SelectedField,
    ) -> Option<(&'static str, Json)> {
        use super::introspection;
        match field.name.as_str() {
            introspection::TYPENAME_FIELD => {
                let named = introspection::type_name(self.introspection_document(), type_name);
                Some(("", named))
            }
            introspection::SCHEMA_FIELD if type_name == self.query_root => Some((
                introspection::SCHEMA_TYPE,
                self.introspection_document().clone(),
            )),
            introspection::TYPE_FIELD if type_name == self.query_root => {
                let name = field
                    .arguments
                    .get(introspection::TYPE_NAME_ARGUMENT)
                    .and_then(Json::as_str)
                    .unwrap_or_default()
                    .to_string();
                let reported = introspection::named_type(self.introspection_document(), &name);
                Some((introspection::TYPE_TYPE, reported))
            }
            _ => None,
        }
    }

    /// `value` where this request's result may still hold it, and `null` where it may not —
    /// spending what it holds, and reporting the field that met the bound.
    ///
    /// Weighed as the bytes the value CARRIES, through the same serializer that writes the
    /// response, so what is counted is what the result holds. Every value the execution
    /// places in the result is weighed here, at every level, because what a nested selection
    /// accumulates is as much the request's choice as the width of one level is.
    fn affordable_answer(&mut self, value: Json, path: &[Json]) -> Json {
        let weight = ::serde_json::to_string(&value)
            .map(|serialized| serialized.len())
            .unwrap_or(usize::MAX);
        if weight > self.answer_bytes {
            self.fault(field_error(
                path,
                &format!("{TOO_MUCH_ANSWER} {MAX_ANSWER_BYTES} bytes."),
            ));
            return Json::Null;
        }
        self.answer_bytes -= weight;
        value
    }

    /// Whether the introspection answer `value` fits in what this request may still be given,
    /// spending it if so.
    ///
    /// Weighed as the bytes the answer CARRIES — the length of the value's own serialized
    /// form, through the same serializer that writes the response — so what is counted is
    /// what the request would be given. A value that cannot be serialized at all is one this
    /// request is not given either.
    fn affordable_introspection(&mut self, value: &Json) -> bool {
        let Ok(serialized) = ::serde_json::to_string(value) else {
            return false;
        };
        let weight = serialized.len();
        if weight > self.introspected_bytes {
            return false;
        }
        self.introspected_bytes -= weight;
        true
    }

    /// The API's type system as introspection reports it, rendered at most ONCE per request:
    /// one GraphQL request is executed against one schema, so a document selecting many
    /// introspection fields reads one snapshot of it rather than rendering it per field.
    ///
    /// `Json::Null` for an API that holds no type system, and for one this service no longer
    /// holds at all.
    fn introspection_document(&mut self) -> &Json {
        if self.introspected.is_none() {
            let rendered = {
                let store = self.context.state.lock();
                store
                    .graphql_apis
                    .get(&self.context.api_id)
                    .map(super::introspection::schema)
                    .unwrap_or(Json::Null)
            };
            self.introspected = Some(rendered);
        }
        self.introspected.get_or_insert(Json::Null)
    }

    /// The name of the type `field_name` of `type_name` resolves to, or the empty name when
    /// the API's schema declares none — a field whose type is unknown addresses no
    /// resolver, so its sub-selection is taken from the value it resolved to.
    fn field_type_name(&self, type_name: &str, field_name: &str) -> String {
        // A field of one of the INTROSPECTION system's own types resolves to the type that
        // system declares for it, which the API's schema declares nothing about.
        if let Some(named) = super::introspection::field_type(type_name, field_name) {
            return named.to_string();
        }
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
    ///
    /// A request function that called `runtime.earlyReturn(value)` has ENDED the step it
    /// belongs to with `value`: for a UNIT resolver and for one function of a pipeline that is
    /// that step ([`Self::run_step`], after which the pipeline carries on with the next
    /// function), and for the pipeline's own BEFORE step it is the resolver — so the functions
    /// and the after step do not run and `value` is the field's value.
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
                max_batch_size: plan.max_batch_size,
            };
            return self.run_step(&step, &base, &mut stash, None).await;
        }
        // A pipeline runs a step per function it names, and no pipeline of this service runs
        // more of them than AppSync holds one ([`MAX_PIPELINE_STEPS`]).
        if plan.functions.len() > MAX_PIPELINE_STEPS {
            return Err(format!("{TOO_MANY_STEPS} {MAX_PIPELINE_STEPS}.").into());
        }
        // The pipeline's BEFORE step: what it produces is the `ctx.prev.result` the first
        // function is handed.
        let mut previous = match (&plan.code, &plan.templates) {
            (Some(code), _) => {
                let context = step_context(&base, &stash, &Json::Null, None, &Json::Null);
                let evaluated = self
                    .evaluated(code, REQUEST_FUNCTION, &context, &mut stash)
                    .await?;
                if evaluated.early {
                    return Ok(evaluated.value);
                }
                evaluated.value
            }
            (None, Some(templates)) => match &templates.request {
                Some(template) => {
                    let context = step_context(&base, &stash, &Json::Null, None, &Json::Null);
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
                .run_step(step, &base, &mut stash, Some(&previous))
                .await
                .map_err(|failure| failure.prefixed(&step.name))?;
        }
        // And its AFTER step, over what the last function produced.
        match (&plan.code, &plan.templates) {
            (Some(code), _) => {
                let context = step_context(&base, &stash, &previous, Some(&previous), &Json::Null);
                self.evaluated(code, RESPONSE_FUNCTION, &context, &mut stash)
                    .await
                    .map(|evaluated| evaluated.value)
            }
            (None, Some(templates)) => match &templates.response {
                Some(template) => {
                    let context =
                        step_context(&base, &stash, &previous, Some(&previous), &Json::Null);
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
    ///
    /// A request function that called `runtime.earlyReturn(value)` has ENDED this step with
    /// `value`: the data source the step names is not called and the step's response function
    /// does not run, so `value` is the step's result as it stands ([`Evaluated`]).
    ///
    /// A step carrying NEITHER `code` nor mapping templates has no definition to run, and that
    /// is exactly the Direct Lambda resolver AppSync documents: "VTL request mapping templates
    /// are optional when using an Lambda data source", so a step on a data source that IS a
    /// function invokes it over the field's own context document and takes what it returned as
    /// the field's value ([`Self::run_direct`]). A step carrying neither on any other data
    /// source really has nothing to run.
    async fn run_step(
        &mut self,
        step: &PipelineStep,
        base: &FieldContext,
        stash: &mut Json,
        previous: Option<&Json>,
    ) -> Result<Json, Failure> {
        let Some(code) = &step.code else {
            // A definition written as VTL MAPPING TEMPLATES rather than as APPSYNC_JS
            // code, which is the other form `Resolver$requestMappingTemplate` /
            // `$responseMappingTemplate` carry; a step that carries neither is a Direct
            // Lambda resolver where its data source is a function.
            return match &step.templates {
                Some(templates) => {
                    self.run_templates(step, templates, base, stash, previous)
                        .await
                }
                None => self.run_direct(step, base, stash, previous).await,
            };
        };
        let recorded = self.recorded_step(&step.name);
        let context = step_context(base, stash, &Json::Null, previous, &Json::Null);
        let requested = self
            .evaluated(code, REQUEST_FUNCTION, &context, stash)
            .await?;
        if requested.early {
            // The step generated no request for a data source, so its trace carries none:
            // what it produced is the answer the code ended it with.
            self.record_response(recorded, &requested.value);
            return Ok(requested.value);
        }
        self.record_request(recorded, &requested.value);
        // A data source that was CALLED and did not answer is handed to the step's own
        // `ctx.error`, over which the response half still runs; a step no data source was
        // called for is reported to the caller ([`Self::run_templates`] states the rule).
        let (result, error) = match self.data_source_outcome(step, &requested.value).await {
            DataSourceOutcome::Result(result) => (result, Json::Null),
            DataSourceOutcome::Answered(failure) => (Json::Null, failure.reported()),
            DataSourceOutcome::NotCalled(failure) => return Err(failure),
        };
        let context = step_context(base, stash, &result, previous, &error);
        let answered = self
            .evaluated(code, RESPONSE_FUNCTION, &context, stash)
            .await?;
        self.record_response(recorded, &answered.value);
        Ok(answered.value)
    }

    /// Run one step that carries NO definition at all: the Direct Lambda resolver.
    ///
    /// AppSync invokes the data source's function over the whole resolver context document —
    /// the `arguments`, `identity`, `source`, `request`, `prev`, `stash` and `info` a resolver
    /// reads ([`step_context`]) — and the value the function RETURNED is the field's value,
    /// because there is no response definition to map it through. A step whose data source is
    /// not a function has nothing to run.
    async fn run_direct(
        &mut self,
        step: &PipelineStep,
        base: &FieldContext,
        stash: &Json,
        previous: Option<&Json>,
    ) -> Result<Json, Failure> {
        let data_source = self.invokable(step)?;
        let context = direct_event(base, stash, previous);
        let recorded = self.recorded_step(&step.name);
        self.record_request(recorded, &loggable_event(&context));
        let answered = self
            .lambda_results(&data_source, vec![context], batches(step.max_batch_size))
            .await?
            .into_iter()
            .next()
            .unwrap_or_else(|| Err(NOT_ONE_RESULT.to_string().into()))?;
        self.record_response(recorded, &answered);
        Ok(answered)
    }

    /// The data source of `step` where it is one a resolver INVOKES, or the refusal for a step
    /// that carries no definition and resolves against anything else.
    fn invokable(&self, step: &PipelineStep) -> Result<StepDataSource, Failure> {
        let Some(data_source) = &step.data_source else {
            return Err(crate::state::DATA_SOURCE_NAME_MISSING_MESSAGE
                .to_string()
                .into());
        };
        if data_source.kind != LAMBDA_DATA_SOURCE {
            return Err(NO_DEFINITION.to_string().into());
        }
        Ok(data_source.clone())
    }

    /// ONE APPSYNC_JS evaluation of this request, charged against what the request may run
    /// ([`MAX_ENGINE_RUNS`]) and started only inside the span its evaluations share
    /// ([`MAX_ENGINE_RUNTIME`]).
    ///
    /// Every evaluation of a resolver's code goes through here, so one request's runs are
    /// counted in one place however many steps its fields expand into.
    async fn evaluated(
        &mut self,
        code: &str,
        function: &str,
        context: &Json,
        stash: &mut Json,
    ) -> Result<Evaluated, Failure> {
        if self.engine_runs >= MAX_ENGINE_RUNS {
            return Err(format!("{TOO_MANY_ENGINE_RUNS} {MAX_ENGINE_RUNS}.").into());
        }
        if ::std::time::Instant::now() >= self.engine_deadline {
            return Err(format!(
                "{EVALUATIONS_TOOK_TOO_LONG} {:?}.",
                MAX_ENGINE_RUNTIME
            )
            .into());
        }
        self.engine_runs = self.engine_runs.saturating_add(1);
        returned(code, function, context, stash).await
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
    ///
    /// A data source that was CALLED and did not answer is not the end of the step: AppSync
    /// hands what went wrong to the step's own `$ctx.error` and evaluates the RESPONSE half
    /// over it, so a definition that states what to do about a failure decides the field —
    /// which is what `$util.error($ctx.error.message, $ctx.error.type)` is written for. A step
    /// with no response half to run reports the failure itself, and so does one whose data
    /// source was never called at all ([`DataSourceOutcome`]).
    async fn run_templates(
        &mut self,
        step: &PipelineStep,
        templates: &Templates,
        base: &FieldContext,
        stash: &mut Json,
        previous: Option<&Json>,
    ) -> Result<Json, Failure> {
        let recorded = self.recorded_step(&step.name);
        let requested = match &templates.request {
            Some(template) => {
                let context = step_context(base, stash, &Json::Null, previous, &Json::Null);
                let rendered = self.rendered(template, &context, stash).await?;
                read_rendered(&rendered, REQUEST_TEMPLATE)?
            }
            None => Json::Null,
        };
        self.record_request(recorded, &requested);
        let (result, error) = match self.data_source_outcome(step, &requested).await {
            DataSourceOutcome::Result(result) => (result, Json::Null),
            DataSourceOutcome::Answered(failure) if templates.response.is_some() => {
                (Json::Null, failure.reported())
            }
            DataSourceOutcome::Answered(failure) | DataSourceOutcome::NotCalled(failure) => {
                return Err(failure)
            }
        };
        let answered = match &templates.response {
            Some(template) => {
                let context = step_context(base, stash, &result, previous, &error);
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
    /// the local resolver AppSync documents. `AWS_LAMBDA` is a FUNCTION, so the request's
    /// `payload` is the event that function is INVOKED with and what it returned is the
    /// result; the request's `operation` says whether that event travels on its own
    /// (`Invoke`) or as the one-element list a batched invocation carries (`BatchInvoke`). An
    /// `AMAZON_EVENTBRIDGE` data source is CALLED too: the events the request carries are
    /// published onto the bus it was configured with, and the result is what EventBridge
    /// answered ([`super::event_bridge`]). Every other type names a resource of another
    /// service, which this endpoint does not call, so the step is one no data source was
    /// called for rather than one answered with a value nothing produced — the distinction
    /// [`DataSourceOutcome`] draws, and what keeps such a refusal reaching the CALLER instead
    /// of being offered to a definition as a failure it may handle.
    async fn data_source_outcome(
        &mut self,
        step: &PipelineStep,
        requested: &Json,
    ) -> DataSourceOutcome {
        let Some(data_source) = step.data_source.clone() else {
            return DataSourceOutcome::NotCalled(
                crate::state::DATA_SOURCE_NAME_MISSING_MESSAGE
                    .to_string()
                    .into(),
            );
        };
        let payload = || requested.get(PAYLOAD_MEMBER).cloned().unwrap_or(Json::Null);
        match data_source.kind.as_str() {
            LOCAL_DATA_SOURCE => DataSourceOutcome::Result(payload()),
            LAMBDA_DATA_SOURCE => {
                let batched = requested
                    .get(OPERATION_MEMBER)
                    .and_then(Json::as_str)
                    .is_some_and(|operation| operation == BATCH_INVOKE_OPERATION)
                    || batches(step.max_batch_size);
                let invoked = match self
                    .lambda_results(&data_source, vec![payload()], batched)
                    .await
                {
                    Ok(results) => results
                        .into_iter()
                        .next()
                        .unwrap_or_else(|| Err(NOT_ONE_RESULT.to_string().into())),
                    Err(failure) => Err(failure),
                };
                // The function WAS the call, so what it came to — the value it returned, or
                // the error it raised — is the data source's own answer.
                match invoked {
                    Ok(result) => DataSourceOutcome::Result(result),
                    Err(failure) => DataSourceOutcome::Answered(failure),
                }
            }
            EVENT_BRIDGE_DATA_SOURCE => match super::event_bridge::published(
                self.context,
                &super::event_bridge::Configured {
                    name: &data_source.name,
                    event_bus_arn: data_source.event_bus_arn.as_deref(),
                    service_role_arn: data_source.service_role_arn.as_deref(),
                },
                requested,
            )
            .await
            {
                Ok(result) => DataSourceOutcome::Result(result),
                // Carried as a RAISED failure, so the step's own `ctx.error` states the type
                // the refusal is of and an error of the field reports it as its `errorType`.
                Err(refused) => DataSourceOutcome::Answered(Failure::Raised(Box::new(Raised {
                    message: refused.message,
                    error_type: Some(refused.error_type),
                    ..Raised::default()
                }))),
            },
            other => DataSourceOutcome::NotCalled(
                format!(
                    "{DATA_SOURCE_NOT_CALLED} {other} data source {}.",
                    data_source.name
                )
                .into(),
            ),
        }
    }

    /// INVOKE the function `data_source` names once, over `events`, and answer one result per
    /// event.
    ///
    /// `batched` says what the event DOCUMENT is: the LIST of the events, which is what the
    /// `BatchInvoke` operation carries and what the function then answers one element per, or
    /// the single event of the one resolution, which is `Invoke`. So a batched invocation that
    /// answers anything but a list of exactly as many elements as it was given has answered
    /// nothing usable for the siblings it merged, which is reported rather than guessed at.
    ///
    /// The invocation is made only where the data source's own service role may make it
    /// ([`lambda::role_refusal`]) — "the system assumes this role when accessing the data
    /// source" — and a run whose handler RAISED is reported as the error that handler raised:
    /// the class the runtime named for it and its own sentence.
    async fn lambda_results(
        &mut self,
        data_source: &StepDataSource,
        events: Vec<Json>,
        batched: bool,
    ) -> Result<Vec<Result<Json, Failure>>, Failure> {
        let Some(configured) = data_source.function_arn.as_deref() else {
            return Err(
                crate::state::missing_data_source_config_message(LAMBDA_CONFIG_MEMBER).into(),
            );
        };
        let Some(target) = lambda::function_target(configured) else {
            return Err(format!("{NOT_A_FUNCTION} {configured}").into());
        };
        if let Some(refused) = lambda::role_refusal(
            &self.context.engine,
            data_source.service_role_arn.as_deref(),
            &target.arn,
        ) {
            return Err(Failure::Raised(Box::new(Raised {
                message: refused,
                error_type: Some(LAMBDA_INVOCATION_ERROR_TYPE.to_string()),
                ..Raised::default()
            })));
        }
        let document = match batched {
            true => Json::Array(events.clone()),
            false => events.first().cloned().unwrap_or(Json::Null),
        };
        let ran = match lambda::invoke(&self.context.callback, &target, &document).await {
            Ok(ran) => ran,
            Err(unreached) => return Err(unreached.reason().into()),
        };
        if ran.failed() {
            let (class, message) = ran.raised();
            return Err(Failure::Raised(Box::new(Raised {
                message,
                error_type: Some(class),
                ..Raised::default()
            })));
        }
        let answered = ran.value();
        if !batched {
            return Ok(vec![Ok(answered)]);
        }
        let Some(elements) = answered.as_array().filter(|list| list.len() == events.len()) else {
            return Err(format!("{NOT_A_BATCHED_ANSWER} {}.", events.len()).into());
        };
        Ok(elements.iter().cloned().map(batched_item).collect())
    }

    /// The resolver of `field` where it BATCHES its siblings' resolutions with this one, or
    /// `None` where each resolution is an invocation of its own ([`ResolverPlan::batch_size`]).
    fn batched_plan(&self, type_name: &str, field: &SelectedField) -> Option<ResolverPlan> {
        let plan = self.plan(type_name, &field.name)?;
        plan.batch_size().map(|_| plan)
    }

    /// The groups of resolutions ONE invocation of a batched resolver each carries: the plan the
    /// group is resolved by, and the resolutions it merges.
    ///
    /// Two resolutions at one LEVEL of the result are MERGEABLE when they are resolved the same
    /// way — the same data source, the same definition and the same batching size
    /// ([`ResolverPlan::batch_group`]) — because the invocation they share runs one program over
    /// one data source. They need not be of the same parent and need not be of the same field:
    /// what one invocation carries is a resolution per selection, as many as the resolver's own
    /// maximum batching size, and the resolutions past it make a further group. So the
    /// resolutions of one batched field across the elements one list resolved to are one
    /// invocation, which is the batching AppSync's own guide is written about.
    ///
    /// The gathering itself is bounded and linear in what it gathers, because the NUMBER of
    /// parents at a level is what a resolver's own answer came to rather than anything this
    /// crate fixes: a resolution is a RUN, so no more of them are gathered than this request may
    /// still run ([`MAX_RESOLVER_RUNS`]), and a resolution past that is left to be resolved on
    /// its own and to report the bound it met. Each selection's group key is derived ONCE and the
    /// group still open for it is looked up by that key, so no gathered group is ever rescanned.
    fn batch_groups(
        &self,
        type_name: &str,
        parents: &[(Vec<Json>, Json)],
        fields: &[SelectedField],
    ) -> Vec<(ResolverPlan, Vec<BatchMember>)> {
        let mut groups: Vec<(ResolverPlan, Vec<BatchMember>)> = Vec::new();
        // Where the group still OPEN for one key sits, so a member is placed without walking
        // the groups gathered so far.
        let mut open: BTreeMap<BatchKey, usize> = BTreeMap::new();
        let affordable = MAX_RESOLVER_RUNS.saturating_sub(self.runs);
        let mut gathered = 0usize;
        for (at, field) in fields.iter().enumerate() {
            let Some(plan) = self.batched_plan(type_name, field) else {
                continue;
            };
            let Some(key) = plan.batch_group() else {
                continue;
            };
            let size = plan.batch_size().unwrap_or(1);
            for parent in 0..parents.len() {
                if gathered >= affordable {
                    return groups;
                }
                let member = BatchMember { parent, field: at };
                let placed = open.get(&key).copied().filter(|at| {
                    groups
                        .get(*at)
                        .is_some_and(|(_, members)| members.len() < size)
                });
                match placed {
                    Some(at) => {
                        if let Some((_, members)) = groups.get_mut(at) {
                            members.push(member);
                        }
                    }
                    None => {
                        open.insert(key.clone(), groups.len());
                        groups.push((plan.clone(), vec![member]));
                    }
                }
                gathered = gathered.saturating_add(1);
            }
        }
        groups
    }

    /// Resolve the `members` of ONE group — the resolutions a batched resolver merges — in one
    /// invocation of its data source, answering what each of them came to, in their own order.
    ///
    /// Each member's REQUEST half is run on its own, over its own context and its own stash,
    /// because each resolves one field of one parent; what they produce is merged into the ONE
    /// event list the invocation carries, and each member's RESPONSE half then runs over the
    /// element of the answer that is its own. A member whose own half raised, and one whose
    /// element reported an error of its own item, is the only member that did not resolve.
    async fn batched_results(
        &mut self,
        plan: &ResolverPlan,
        type_name: &str,
        parents: &[(Vec<Json>, Json)],
        fields: &[SelectedField],
        members: &[BatchMember],
    ) -> Vec<Result<Json, Failure>> {
        let Some((mut step, in_pipeline)) = plan.batched_step() else {
            return members
                .iter()
                .map(|_| Err(NO_DEFINITION.to_string().into()))
                .collect();
        };
        step.name = members
            .first()
            .and_then(|member| fields.get(member.field))
            .map(|field| field.name.clone())
            .unwrap_or_default();
        // One resolution is one run, batched or not, so the whole group is charged here.
        if self.runs.saturating_add(members.len()) > MAX_RESOLVER_RUNS {
            return members
                .iter()
                .map(|_| Err(format!("{TOO_MANY_RUNS} {MAX_RESOLVER_RUNS}.").into()))
                .collect();
        }
        self.runs = self.runs.saturating_add(members.len());
        // Where in the result each member sits, which is what an error of it is reported under.
        let paths: Vec<Vec<Json>> = members
            .iter()
            .map(|member| {
                let mut path = parents
                    .get(member.parent)
                    .map(|(path, _)| path.clone())
                    .unwrap_or_default();
                if let Some(field) = fields.get(member.field) {
                    path.push(json!(field.key));
                }
                path
            })
            .collect();
        // Every member's request half, each over its own context and stash.
        let mut prepared: Vec<Result<(FieldContext, Json, Evaluated), Failure>> =
            Vec::with_capacity(members.len());
        for (member, path) in members.iter().zip(&paths) {
            let Some((field, source)) = fields
                .get(member.field)
                .zip(parents.get(member.parent).map(|(_, source)| source))
            else {
                prepared.push(Err(NOT_ONE_RESULT.to_string().into()));
                continue;
            };
            let base = self.field_context(type_name, source, field);
            let mut stash = Json::Object(JsonMap::new());
            let requested = self
                .batched_request(&step, &base, &mut stash, in_pipeline)
                .await;
            // What this member's own templates APPENDED is reported under its own path, which
            // is what appending an error is for.
            for appended in ::std::mem::take(&mut self.appended) {
                self.fault(appended.document(path));
            }
            prepared.push(requested.map(|requested| (base, stash, requested)));
        }
        let events: Vec<Json> = prepared
            .iter()
            .filter_map(|ready| ready.as_ref().ok())
            // A member whose request half ended its step early is already answered, so it
            // waits on no invocation and contributes no event to one.
            .filter(|(_, _, requested)| !requested.early)
            .map(|(_, _, requested)| match step.has_definition() {
                // A definition RENDERED the request, so the event is its `payload`; a Direct
                // Lambda resolver has none, so the event is the context document itself.
                true => requested
                    .value
                    .get(PAYLOAD_MEMBER)
                    .cloned()
                    .unwrap_or(Json::Null),
                false => requested.value.clone(),
            })
            .collect();
        let Some(data_source) = step.data_source.clone() else {
            return members
                .iter()
                .map(|_| {
                    Err(crate::state::DATA_SOURCE_NAME_MISSING_MESSAGE
                        .to_string()
                        .into())
                })
                .collect();
        };
        // WHICH operation the invocation is: `BatchInvoke` carries the events of every member
        // in ONE list, and `Invoke` carries one member's event on its own. A batching size
        // configures the first outright; a definition that rendered the request names the
        // operation itself, and it is batched only where EVERY member's own request named the
        // batched one.
        let batched = match step.has_definition() {
            true => prepared.iter().all(|ready| {
                ready.as_ref().ok().is_none_or(|(_, _, requested)| {
                    // A member that is already answered names no operation of its own, so it
                    // decides nothing about how the others are invoked.
                    requested.early
                        || requested
                            .value
                            .get(OPERATION_MEMBER)
                            .and_then(Json::as_str)
                            .is_some_and(|operation| operation == BATCH_INVOKE_OPERATION)
                        || batches(step.max_batch_size)
                })
            }),
            false => batches(step.max_batch_size),
        };
        let invoked = match (events.is_empty(), batched) {
            // Nothing to invoke for: no member produced an event at all.
            (true, _) => Ok(Vec::new()),
            (false, true) => self.lambda_results(&data_source, events, true).await,
            // Not one invocation after all: each member's own event travels on its own.
            (false, false) => {
                let mut answered = Vec::with_capacity(events.len());
                for event in events {
                    let one = self.lambda_results(&data_source, vec![event], false).await;
                    answered.push(match one {
                        Ok(results) => results
                            .into_iter()
                            .next()
                            .unwrap_or_else(|| Err(NOT_ONE_RESULT.to_string().into())),
                        Err(failure) => Err(failure),
                    });
                }
                Ok(answered)
            }
        };
        let mut answers = match invoked {
            Ok(answers) => answers.into_iter(),
            // The invocation itself produced nothing, so no member of the group resolved.
            Err(failure) => {
                return prepared
                    .into_iter()
                    .map(|ready| Err(ready.err().unwrap_or_else(|| failure.clone())))
                    .collect()
            }
        };
        let mut resolved = Vec::with_capacity(members.len());
        for (path, ready) in paths.iter().zip(prepared) {
            let (base, mut stash, requested) = match ready {
                Ok(ready) => ready,
                Err(failure) => {
                    resolved.push(Err(failure));
                    continue;
                }
            };
            if requested.early {
                // This member's own request half ended its step with that value, so there is
                // no answer of the invocation to take for it and no response half to run.
                resolved.push(Ok(requested.value));
                continue;
            }
            let result = match answers.next() {
                Some(Ok(result)) => result,
                Some(Err(failure)) => {
                    resolved.push(Err(failure));
                    continue;
                }
                None => {
                    resolved.push(Err(NOT_ONE_RESULT.to_string().into()));
                    continue;
                }
            };
            let answered = self
                .batched_response(&step, &base, &mut stash, &result, in_pipeline)
                .await;
            for appended in ::std::mem::take(&mut self.appended) {
                self.fault(appended.document(path));
            }
            resolved.push(answered);
        }
        resolved
    }

    /// The REQUEST half of one member of a batched resolution: what the step's definition
    /// renders for it, or — for a Direct Lambda resolver — the field's own context document.
    ///
    /// A request function that called `runtime.earlyReturn(value)` has ENDED this member's
    /// step: the answer is reported `early` ([`Evaluated`]), and the caller then neither
    /// invokes the data source for that member nor runs its response half.
    ///
    /// `in_pipeline` says whether the step is a FUNCTION of a pipeline, which is what decides
    /// whether the resolution has a previous step at all.
    async fn batched_request(
        &mut self,
        step: &PipelineStep,
        base: &FieldContext,
        stash: &mut Json,
        in_pipeline: bool,
    ) -> Result<Evaluated, Failure> {
        let previous = in_pipeline.then_some(&Json::Null);
        match (&step.code, &step.templates) {
            (Some(code), _) => {
                let context = step_context(base, stash, &Json::Null, previous, &Json::Null);
                self.evaluated(code, REQUEST_FUNCTION, &context, stash)
                    .await
            }
            (None, Some(templates)) => match &templates.request {
                Some(template) => {
                    let context = step_context(base, stash, &Json::Null, previous, &Json::Null);
                    let rendered = self.rendered(template, &context, stash).await?;
                    read_rendered(&rendered, REQUEST_TEMPLATE).map(Evaluated::plain)
                }
                None => Ok(Evaluated::plain(Json::Null)),
            },
            (None, None) => Ok(Evaluated::plain(direct_event(base, stash, previous))),
        }
    }

    /// The RESPONSE half of one member, over the element of the batched answer that is its own:
    /// what the step's definition makes of that element, or the element itself for a Direct
    /// Lambda resolver, which has no response definition to map it through.
    async fn batched_response(
        &mut self,
        step: &PipelineStep,
        base: &FieldContext,
        stash: &mut Json,
        result: &Json,
        in_pipeline: bool,
    ) -> Result<Json, Failure> {
        // The element this sibling is handed IS the data source's answer for it: a sibling
        // whose own element reported an error is reported before its response half would run
        // ([`batched_item`]), so nothing of a failure reaches this `ctx`.
        let context = step_context(
            base,
            stash,
            result,
            in_pipeline.then_some(&Json::Null),
            &Json::Null,
        );
        match (&step.code, &step.templates) {
            (Some(code), _) => {
                self.evaluated(code, RESPONSE_FUNCTION, &context, stash)
                    .await
                    .map(|evaluated| evaluated.value)
            }
            (None, Some(templates)) => match &templates.response {
                Some(template) => {
                    let rendered = self.rendered(template, &context, stash).await?;
                    read_rendered(&rendered, RESPONSE_TEMPLATE)
                }
                None => Ok(result.clone()),
            },
            (None, None) => Ok(result.clone()),
        }
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
                "selectionSetGraphQL": selection_set_graphql(&field.selections, 0),
            }),
        }
    }
}

/// What resolving one step's DATA SOURCE came to.
///
/// The three cases are not one: a data source that was CALLED and did not answer is a failure
/// AppSync hands to the step's own `ctx.error`, over which the response half still runs, so a
/// definition that states what to do about it decides the field. A step whose data source was
/// never called — the API holds none under the name the step names, or its type names a
/// resource of a service this endpoint does not call — produced no such answer: there is
/// nothing for a definition to handle, and reporting it as one would answer a client who asked
/// for work this endpoint did not do with whatever value a response half happened to render.
/// Such a step is reported to the CALLER as a field that did not resolve.
enum DataSourceOutcome {
    /// The data source produced this result.
    Result(Json),
    /// The data source was called and answered with this failure.
    Answered(Failure),
    /// No data source was called at all.
    NotCalled(Failure),
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
    /// "The maximum batching size for a resolver" (`Resolver$maxBatchSize`): how many field
    /// resolutions of this resolver one invocation of its data source may carry, and — since
    /// "the default is 0", which batches nothing — whether it batches at all.
    max_batch_size: i32,
}

impl ResolverPlan {
    /// The step ONE batched invocation of this resolver runs, and whether that step is a
    /// FUNCTION of a pipeline — or `None` for a resolver that is not batched.
    ///
    /// A resolver batches only where its step configures a maximum batching size
    /// (`maxBatchSize`, whose default of 0 batches nothing) and its data source IS a function,
    /// because batching is the `BatchInvoke` operation of a Lambda data source. Which step that
    /// is depends on the kind: a UNIT resolver's own definition is the step, while a PIPELINE
    /// resolver invokes functions and "the maximum batching size for a function"
    /// (`FunctionConfiguration$maxBatchSize`) is each function's own — so a pipeline batches
    /// where it is ONE such function and the resolver's own before and after steps add nothing
    /// of their own to it.
    fn batched_step(&self) -> Option<(PipelineStep, bool)> {
        let invokes_a_function = |step: &PipelineStep| {
            step.data_source
                .as_ref()
                .is_some_and(|data_source| data_source.kind == LAMBDA_DATA_SOURCE)
        };
        if !self.is_pipeline {
            let step = PipelineStep {
                name: String::new(),
                code: self.code.clone(),
                templates: self.templates.clone(),
                data_source: self.data_source.clone(),
                max_batch_size: self.max_batch_size,
            };
            // A batching size configures `BatchInvoke` outright. A resolver whose own
            // DEFINITION renders the request may name that operation itself instead, and which
            // operation it named is known only once that definition has run — so such a
            // resolver's sibling resolutions are gathered too, and what they rendered is what
            // decides whether one invocation carries them all
            // ([`Execution::batched_results`]).
            let gathers = batches(step.max_batch_size) || step.has_definition();
            return (invokes_a_function(&step) && gathers).then_some((step, false));
        }
        if self.code.is_some() || self.templates.is_some() {
            return None;
        }
        let [function] = self.functions.as_slice() else {
            return None;
        };
        (invokes_a_function(function) && batches(function.max_batch_size))
            .then(|| (function.clone(), true))
    }

    /// How many resolutions ONE invocation of this resolver's data source carries, or `None`
    /// for a resolver that is not batched.
    ///
    /// The resolver's own maximum batching size where it configures one; else as many as a
    /// resolver may configure ([`MAX_BATCHED_RESOLUTIONS`]), which is what a `BatchInvoke` a
    /// definition names for itself carries.
    fn batch_size(&self) -> Option<usize> {
        let (step, _) = self.batched_step()?;
        Some(match step.max_batch_size > 0 {
            true => usize::try_from(step.max_batch_size).unwrap_or(MAX_BATCHED_RESOLUTIONS),
            false => MAX_BATCHED_RESOLUTIONS,
        })
    }

    /// What makes two sibling resolutions MERGEABLE into one invocation: the data source the
    /// invocation is made on, the definition the invocation's event is produced by (none for a
    /// Direct Lambda resolver, which has one) and the batching size that invocation carries at
    /// most. Two resolutions that agree on all of it run one program over one data source,
    /// which is what one invocation can carry.
    fn batch_group(&self) -> Option<BatchKey> {
        let (step, in_pipeline) = self.batched_step()?;
        let templates = step.templates.unwrap_or(Templates {
            request: None,
            response: None,
        });
        Some((
            step.data_source
                .map(|data_source| data_source.name)
                .unwrap_or_default(),
            step.code,
            templates.request,
            templates.response,
            step.max_batch_size,
            in_pipeline,
        ))
    }
}

/// What two resolutions have to AGREE ON to be merged into one invocation
/// ([`ResolverPlan::batch_group`]): the data source, the definition that produces the event and
/// the batching size, and whether the step is a function of a pipeline.
type BatchKey = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    i32,
    bool,
);

/// ONE resolution a batched invocation carries: which selection it is of, and which of the
/// parents at that level of the result it belongs to.
#[derive(Debug, Clone, Copy)]
struct BatchMember {
    /// The parent whose selection this resolution is, as its position in the level's parents.
    parent: usize,
    /// The selection this resolution is of, as its position in the selection set.
    field: usize,
}

/// One step of a resolution: a UNIT resolver, or one function of a pipeline.
#[derive(Debug, Clone)]
struct PipelineStep {
    /// What the step is called, which is what an error of the step names it by.
    name: String,
    code: Option<String>,
    templates: Option<Templates>,
    data_source: Option<StepDataSource>,
    /// The maximum batching size the definition this step came from configures, which says
    /// whether its data source is invoked with the BATCHED event document.
    max_batch_size: i32,
}

impl PipelineStep {
    /// Whether this step carries a DEFINITION at all: the `code` a resolver or a function is
    /// written in, or the mapping templates that are the other form of one. A step carrying
    /// neither is the Direct Lambda resolver AppSync documents, where the field's own context
    /// document IS the event its data source is invoked with.
    fn has_definition(&self) -> bool {
        self.code.is_some() || self.templates.is_some()
    }
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

    /// This failure as the step's own `ctx.error`: the `message` of what went wrong and the
    /// `type` it is of, which is what a definition reads to decide the field itself.
    fn reported(&self) -> Json {
        match self {
            Self::Message(message) => json!({"message": message, "type": Json::Null}),
            Self::Raised(raised) => json!({
                "message": raised.message,
                "type": raised.error_type.clone(),
            }),
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

/// Whether a definition configuring the maximum batching size `max_batch_size` is BATCHED at
/// all: "the default is 0" (`Resolver$maxBatchSize`), which batches nothing, so only a
/// positive size asks for the `BatchInvoke` operation and the list event document it carries.
fn batches(max_batch_size: i32) -> bool {
    max_batch_size > 0
}

/// ONE element of a batched answer, as the result of the sibling it belongs to.
///
/// An element that REPORTS AN ERROR OF ITS OWN ITEM — an object carrying `errorMessage`, with
/// `errorType` naming its class where the function named one — is the one sibling that did not
/// resolve: it reports that error under its own path and resolves to nothing, and no other
/// sibling of the invocation is affected. Every other element is the value its sibling takes.
fn batched_item(element: Json) -> Result<Json, Failure> {
    let member = |name: &str| {
        element
            .get(name)
            .and_then(Json::as_str)
            .map(str::to_string)
            .filter(|text| !text.is_empty())
    };
    match member(ITEM_ERROR_MESSAGE_MEMBER) {
        Some(message) => Err(Failure::Raised(Box::new(Raised {
            message,
            error_type: member(ITEM_ERROR_TYPE_MEMBER),
            ..Raised::default()
        }))),
        None => Ok(element),
    }
}

/// The data source one step resolves against: its type, the name it is held under, and what a
/// step that CALLS it needs of its configuration — for a data source that IS a function, what
/// addresses that function; for one that is an event BUS, the bus its events go to; and, for
/// either, whose role the call is made under.
#[derive(Debug, Clone)]
struct StepDataSource {
    kind: String,
    name: String,
    /// "The Amazon Resource Name (ARN) for the Lambda function"
    /// (`LambdaDataSourceConfig$lambdaFunctionArn`), for a data source configured with one.
    function_arn: Option<String>,
    /// `eventBridgeConfig.eventBusArn` of an `AMAZON_EVENTBRIDGE` data source, and `None` for
    /// every other type — which configures no bus.
    event_bus_arn: Option<String>,
    /// "The Identity and Access Management (IAM) service role Amazon Resource Name (ARN) for
    /// the data source. The system assumes this role when accessing the data source"
    /// (`DataSource$serviceRoleArn`) — so it is the principal whose permissions admit the
    /// call a step makes.
    service_role_arn: Option<String>,
}

/// The data source `name` of `api`, or `None` when the API holds none under that name.
///
/// Copied out of the record as it stands NOW, so a data source an update REPOINTED is called
/// with the configuration it carries at the moment the step runs.
fn step_data_source(api: &GraphqlApiRecord, name: Option<&str>) -> Option<StepDataSource> {
    let name = name?;
    api.data_sources
        .get(name)
        .map(|record: &DataSourceRecord| StepDataSource {
            kind: record.r#type.clone(),
            name: record.name.clone(),
            event_bus_arn: record
                .event_bridge_config
                .as_ref()
                .map(|config| config.event_bus_arn.clone()),
            function_arn: record
                .lambda_config
                .as_ref()
                .map(|config| config.lambda_function_arn.clone()),
            service_role_arn: record.service_role_arn.clone(),
        })
}

/// The steps a `PIPELINE` resolver invokes, in the order its `pipelineConfig` names them.
///
/// A function the API no longer holds contributes a step with no definition, which is
/// reported as a step that could not run rather than skipped: the pipeline names it.
///
/// At most one step past [`MAX_PIPELINE_STEPS`] is built, so a stored pipeline naming more of
/// them than any AppSync pipeline holds costs nothing to read and is reported as the bound it
/// met ([`Execution::run`]) rather than run with the steps that fit.
fn pipeline_steps(api: &GraphqlApiRecord, resolver: &ResolverRecord) -> Vec<PipelineStep> {
    resolver
        .pipeline_config
        .as_ref()
        .and_then(|pipeline| pipeline.functions.clone())
        .unwrap_or_default()
        .iter()
        .take(MAX_PIPELINE_STEPS.saturating_add(1))
        .map(|function_id| match api.functions.get(function_id) {
            Some(function) => PipelineStep {
                name: function.name.clone(),
                code: function.code.clone(),
                templates: Templates::of(
                    function.request_mapping_template.as_deref(),
                    function.response_mapping_template.as_deref(),
                ),
                data_source: step_data_source(api, Some(&function.data_source_name)),
                max_batch_size: function.max_batch_size,
            },
            None => PipelineStep {
                name: function_id.clone(),
                code: None,
                templates: None,
                data_source: None,
                max_batch_size: 0,
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

/// The event a DIRECT Lambda resolver's data source is invoked with: the resolver context
/// document of the field being resolved.
///
/// It carries the members a Direct Lambda resolver's event has and no others — the field's
/// `arguments`, who the request presented itself as (`identity`), the value its parent resolved
/// to (`source`), the request's own fields (`request`), where the field sits (`info`), the
/// `stash` the resolution carries and the previous step's result (`prev`) — because this
/// document IS the event, not the `$context` a definition is evaluated over
/// ([`step_context`], which also carries the aliases and the result members that context
/// has).
///
/// `previous` is `None` for a resolution that is not a step of a PIPELINE, and `prev` is then
/// nothing at all: there is no previous step for it to be the result of.
fn direct_event(base: &FieldContext, stash: &Json, previous: Option<&Json>) -> Json {
    json!({
        "arguments": base.arguments,
        "identity": base.identity,
        "source": base.source,
        "request": base.request,
        "info": base.info,
        "stash": stash,
        "prev": previous.map_or(Json::Null, |result| json!({"result": result})),
    })
}

/// The selection set `selections` as `info.selectionSetGraphQL` states it: the GraphQL text of
/// the selection, as the request's own document wrote it.
///
/// `info.selectionSetList` is the NAMES of the selected fields and this is the SELECTION itself,
/// so it carries what a name does not: the alias a selection answers under where it carries one,
/// and the arguments it selects with. A field that selects nothing — a scalar — has no selection
/// set at all and the text is empty; every other selection is a brace-delimited block, one
/// selected field per line, with each field's own sub-selection nested inside it.
fn selection_set_graphql(selections: &[SelectedField], depth: usize) -> String {
    if selections.is_empty() {
        return String::new();
    }
    let indent = "  ".repeat(depth.saturating_add(1));
    let closing = "  ".repeat(depth);
    let mut written = String::from("{\n");
    for selected in selections {
        written.push_str(&indent);
        // "alias: field" where the selection answers under a key of its own.
        if selected.key != selected.name {
            written.push_str(&selected.key);
            written.push_str(": ");
        }
        written.push_str(&selected.name);
        written.push_str(&written_arguments(&selected.arguments));
        let nested = selection_set_graphql(&selected.selections, depth.saturating_add(1));
        if !nested.is_empty() {
            written.push(' ');
            written.push_str(&nested);
        }
        written.push('\n');
    }
    written.push_str(&closing);
    written.push('}');
    written
}

/// The arguments one selection carries, as GraphQL writes an argument list — `(name: value, …)`,
/// and nothing at all for a selection that carries none.
///
/// The values are the ones the selection resolved to, the request's own variables substituted,
/// so each is written as the GraphQL literal of that value ([`written_value`]).
fn written_arguments(arguments: &JsonMap<String, Json>) -> String {
    if arguments.is_empty() {
        return String::new();
    }
    let written: Vec<String> = arguments
        .iter()
        .map(|(name, value)| format!("{name}: {}", written_value(value)))
        .collect();
    format!("({})", written.join(", "))
}

/// One argument value as GraphQL writes a literal of it.
///
/// GraphQL's literals are JSON's but for an INPUT OBJECT, whose member names are written bare
/// rather than quoted — so a value is written through the one serializer this crate writes JSON
/// with, and an object member by member.
fn written_value(value: &Json) -> String {
    match value {
        Json::Object(members) => {
            let written: Vec<String> = members
                .iter()
                .map(|(name, member)| format!("{name}: {}", written_value(member)))
                .collect();
            format!("{{{}}}", written.join(", "))
        }
        Json::Array(elements) => {
            let written: Vec<String> = elements.iter().map(written_value).collect();
            format!("[{}]", written.join(", "))
        }
        other => other.to_string(),
    }
}

/// `event` as a field-level LOG record may carry it: the event MINUS the request's own fields.
///
/// A record is read by whoever can read the API's log group and is kept for that group's whole
/// retention, and the request's own fields are where a caller presents its authorization
/// material — so no record carries them, which is the same reading the context a verbose record
/// keeps is built on ([`logged_context`]). Only the value handed to the data source carries
/// them, because that is the event AppSync invokes the function with.
fn loggable_event(event: &Json) -> Json {
    let mut reduced = event.clone();
    if let Some(members) = reduced.as_object_mut() {
        members.remove(REQUEST_MEMBER);
    }
    reduced
}

/// The member of a resolver context document that carries the REQUEST's own fields, which a
/// log record leaves out ([`loggable_event`]).
const REQUEST_MEMBER: &str = "request";

/// The `ctx` one step is run with: what the field carries, the stash as the steps before it
/// left it, the result the data source produced, the previous step's result and what went
/// wrong with the data source.
///
/// The member names are the ones APPSYNC_JS code reads them under: `arguments` (with `args`
/// as its documented alias), `source`, `identity`, `request`, `info`, `stash`, `result`,
/// `prev` and `error` — the last of which carries the `message` and the `type` of a data
/// source that did not answer, and is `null` for a step whose data source did.
fn step_context(
    base: &FieldContext,
    stash: &Json,
    result: &Json,
    previous: Option<&Json>,
    error: &Json,
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
        "error": error,
    })
}

/// What ONE evaluation of a resolver's code produced: the value it answered, and whether it
/// ended the step it belongs to EARLY.
///
/// `runtime.earlyReturn(value)` "returns prematurely from a request function", so a request
/// function that called it has ended the step it belongs to: nothing else of that step runs.
/// A plain return is the step's request half answering the configuration its data source is
/// then called with, which is a different thing, so the two are carried apart.
#[derive(Debug, Clone)]
struct Evaluated {
    /// The value the evaluation answered.
    value: Json,
    /// Whether the code answered it through `runtime.earlyReturn`.
    early: bool,
}

impl Evaluated {
    /// A value a definition produced the ORDINARY way, which ends no step of its own: what a
    /// mapping template renders, and the context document a step with no definition at all is
    /// invoked over.
    fn plain(value: Json) -> Self {
        Self {
            value,
            early: false,
        }
    }
}

/// Run `function` of `code` over `context` and answer what it returned, carrying the stash
/// it left behind onto `stash`.
///
/// `Err` is a step that did not return: the code raised (`util.error` is how a resolver
/// refuses), it defines no such function, or one of the engine's bounds stopped it. The text
/// is the engine's own, and a raise that NAMED an `errorType` carries it — `util.error`'s
/// second argument is "a custom error type" the error of the field reports as its own.
async fn returned(
    code: &str,
    function: &str,
    context: &Json,
    stash: &mut Json,
) -> Result<Evaluated, Failure> {
    let evaluation =
        appsync_js::evaluate(code.to_string(), function.to_string(), context.clone()).await;
    if let Some(left) = evaluation
        .stash
        .as_deref()
        .and_then(|text| ::serde_json::from_str::<Json>(text).ok())
    {
        *stash = left;
    }
    let read = |text: &str| ::serde_json::from_str(text).unwrap_or(Json::Null);
    match evaluation.outcome {
        Outcome::Returned(text) => Ok(Evaluated {
            value: read(&text),
            early: false,
        }),
        Outcome::ReturnedEarly(text) => Ok(Evaluated {
            value: read(&text),
            early: true,
        }),
        Outcome::Raised {
            message,
            error_type: Some(error_type),
        } => Err(Failure::Raised(Box::new(Raised {
            message,
            error_type: Some(error_type),
            ..Raised::default()
        }))),
        Outcome::Raised { message, .. } | Outcome::Stopped(message) => {
            Err(Failure::Message(message))
        }
        Outcome::Faults(faults) => Err(Failure::Message(
            faults
                .first()
                .map(|fault| fault.value.clone())
                .unwrap_or_else(|| NO_DEFINITION.to_string()),
        )),
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
const TOO_MANY_ENGINE_RUNS: &str = "One request's fields run the APPSYNC_JS runtime at most";
const TOO_MANY_STEPS: &str = "A pipeline resolver of this endpoint runs at most";
const EVALUATIONS_TOOK_TOO_LONG: &str =
    "This request's resolvers ran for longer than this endpoint evaluates them in:";
const TOO_MANY_ERRORS: &str = "This request's result reports at most";
const TOO_MUCH_ARGUMENT: &str = "One request's selected arguments hold at most";
const TOO_MUCH_INTROSPECTION: &str = "One request's introspection answers hold at most";
const TOO_MUCH_ANSWER: &str = "One request's result holds at most";
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

/// The member a data source whose type is `AWS_LAMBDA` is configured through, which is what a
/// data source holding none is reported as missing.
const LAMBDA_CONFIG_MEMBER: &str = "lambdaConfig";

/// The sentences a Lambda data source's own refusals state: a configured value that names no
/// function at all, a batched invocation whose answer is not one element per merged
/// resolution, and an invocation that produced no result for the resolution it was made for.
const NOT_A_FUNCTION: &str = "This data source's lambdaFunctionArn names no Lambda function:";
const NOT_A_BATCHED_ANSWER: &str =
    "The data source's function answered a batched invocation with something other than a list \
     of";
const NOT_ONE_RESULT: &str =
    "The data source's function returned no result for this field's resolution.";

/// The `errorType` AppSync reports for an invocation of a data source's function that COULD
/// NOT BE MADE — the class its errors of that kind carry, under the `Lambda:` prefix every
/// error of a Lambda data source is reported under.
const LAMBDA_INVOCATION_ERROR_TYPE: &str = "Lambda:IllegalArgument";

#[cfg(test)]
mod tests {
    use super::*;

    /// One selected field, as a case writes one.
    fn selected(key: &str, name: &str, arguments: Json, selections: Vec<SelectedField>) -> SelectedField {
        SelectedField {
            key: key.to_string(),
            name: name.to_string(),
            arguments: arguments.as_object().cloned().unwrap_or_default(),
            selections,
        }
    }

    /// `info.selectionSetGraphQL` is the SELECTION, so it carries what the list of names does
    /// not: the alias a selection answers under, the arguments it selects with and the
    /// sub-selection of each selected field. A field that selects nothing has no selection set.
    #[test]
    fn the_selection_set_is_written_as_the_document_wrote_it() {
        // The rows differ along what the selection carries: nothing, plain fields, an alias,
        // arguments of every literal form, and a nested sub-selection.
        let cases: [(&str, Vec<SelectedField>, &str); 6] = [
            ("a scalar field, which selects nothing", Vec::new(), ""),
            (
                "two plain fields",
                vec![
                    selected("id", "id", Json::Null, Vec::new()),
                    selected("name", "name", Json::Null, Vec::new()),
                ],
                "{\n  id\n  name\n}",
            ),
            (
                "a selection answering under an alias",
                vec![selected("mine", "id", Json::Null, Vec::new())],
                "{\n  mine: id\n}",
            ),
            (
                "arguments of each literal form",
                vec![selected(
                    "item",
                    "item",
                    json!({"id": "a", "size": 2, "live": true, "tags": ["x", "y"]}),
                    Vec::new(),
                )],
                "{\n  item(id: \"a\", size: 2, live: true, tags: [\"x\", \"y\"])\n}",
            ),
            (
                "an input object, whose members GraphQL writes bare",
                vec![selected("item", "item", json!({"by": {"id": "a"}}), Vec::new())],
                "{\n  item(by: {id: \"a\"})\n}",
            ),
            (
                "a nested sub-selection",
                vec![selected(
                    "author",
                    "author",
                    Json::Null,
                    vec![
                        selected("name", "name", Json::Null, Vec::new()),
                        selected(
                            "posts",
                            "posts",
                            Json::Null,
                            vec![selected("title", "title", Json::Null, Vec::new())],
                        ),
                    ],
                )],
                "{\n  author {\n    name\n    posts {\n      title\n    }\n  }\n}",
            ),
        ];
        for (case, selections, expected) in cases {
            let written = selection_set_graphql(&selections, 0);
            assert_eq!(
                written, expected,
                "case {case:?}: expected {expected:?}, got {written:?}"
            );
        }
    }

    /// A field-level log record leaves the request's own fields out, whatever else the event
    /// carries, because that is where a caller presents its authorization material.
    #[test]
    fn a_logged_event_carries_everything_but_the_requests_own_fields() {
        let event = json!({
            "arguments": {"id": "a"},
            "identity": {"resolverContext": {"who": "someone"}},
            "source": Json::Null,
            "request": {"headers": {"authorization": "a-token-a-caller-presented"}},
            "info": {"fieldName": "item"},
            "stash": {},
            "prev": Json::Null,
        });
        let logged = loggable_event(&event);
        assert_eq!(
            logged.get("request"),
            None,
            "case a logged event: expected the request's own fields to be left out, got {logged}"
        );
        for member in ["arguments", "identity", "source", "info", "stash", "prev"] {
            assert_eq!(
                logged.get(member),
                event.get(member),
                "case a logged event: expected {member:?} to be carried as the event has it, \
                 got {logged}"
            );
        }
        assert!(
            !logged.to_string().contains("a-token-a-caller-presented"),
            "case a logged event: expected no part of the material a caller presented, got \
             {logged}"
        );
    }
}
