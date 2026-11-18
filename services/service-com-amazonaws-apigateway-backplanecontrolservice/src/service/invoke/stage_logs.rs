// SPDX-License-Identifier: CC0-1.0

//! What a stage LOGS about a request it served: the stage's own access-log record, and the
//! execution log its method settings turn on.
//!
//! Both are CloudWatch Logs deliveries API Gateway makes on the account's behalf, so both
//! need the same two things to be configured, and neither happens without them:
//!
//! * the ACCOUNT's CloudWatch role. `Account$cloudwatchRoleArn` is "the ARN of an Amazon
//!   CloudWatch role for the current Account", which is the role API Gateway assumes to
//!   write into the account's log groups: an account that has never had one set gives API
//!   Gateway nothing to write as, so a stage of it serves its traffic and delivers no
//!   record at all. That is the whole difference between a stage whose logging is wired and
//!   one whose account step was skipped, and it is read here rather than at the control
//!   plane, which accepts a logging configuration whatever the account carries;
//! * the STAGE's own configuration. `Stage$accessLogSettings` carries the
//!   `destinationArn` a record is written to and the `format` it is rendered by, and
//!   `Stage$methodSettings` carries the `loggingLevel`/`dataTrace` that turn the EXECUTION
//!   log on for a method. A stage carrying neither logs nothing; each is independent of
//!   the other, so a stage may carry one without the other.
//!
//! # The access log
//!
//! One record per request the stage served — including a request the api itself refused,
//! which is a request the stage served an answer to — rendered from the stage's own format
//! against this call's `$context` values ([`super::call_context`], the same values a mapping
//! template of the call reads, with what the ANSWER came to added beside them). The
//! rendering and the write are `service_common`'s: the HTTP api plane of API Gateway
//! configures and renders access logging the same way, so one format cannot be rendered two
//! ways.
//!
//! Each stage writes into the destination IT names, so two stages of one api pointed at two
//! groups never cross: the destination is read off the stage the request reached. The
//! destination is a log group of the ACCOUNT's — named by whoever configured the stage — so a
//! stage pointed at a group CloudWatch Logs does not hold delivers nothing and brings no
//! group into existence; the stage's EXECUTION log group below is the one API Gateway
//! publishes itself, and that one it provisions.
//!
//! # The execution log
//!
//! `MethodSetting$loggingLevel` is "specifies the logging level for this method, which
//! affects the log entries pushed to Amazon CloudWatch Logs" with "valid values are `OFF`,
//! `ERROR`, and `INFO`", and `MethodSetting$dataTrace` is "specifies whether data trace
//! logging is enabled for this method, which affects the log entries pushed to Amazon
//! CloudWatch Logs". So the setting in force for the method the request reached decides:
//!
//! * `INFO` — the whole trace of the request is written: every step of serving it, and the
//!   failure where there was one;
//! * `ERROR` — only a request the stage answered with an error has a trace written, which
//!   is what logging errors only means, and that trace carries only the entries that report
//!   the FAILURE: the levels are nested, so the informational steps `INFO` adds — the verb
//!   and path, the header names, the payload lines and the status the method completed with —
//!   are not among them ([`Level`]);
//! * `OFF`, or no setting at all — nothing is written.
//!
//! `dataTrace` is what puts the request's and the response's own payloads in that trace, which
//! is the data the setting exists to gate: turned off, the trace carries no line about a
//! payload at all. With it on, the trace carries one line per TRANSFORMATION POINT the request
//! crossed — the method request before the integration's own request transformation, the
//! integration request as it was sent to the backend after it, the backend's answer as it
//! arrived before the integration response's mapping, and the method response after that
//! mapping ([`Exchanged`] carries the two inner ones, which nothing in the answer the caller
//! holds says). A request that reached no backend at all crosses only the outer two, so those
//! are the only payload lines it leaves. Every payload line is informational, so the data a
//! `dataTrace` adds is data `INFO` writes.
//!
//! The group is the one API Gateway publishes for a stage's execution log,
//! `API-Gateway-Execution-Logs_{rest-api-id}/{stage-name}` ([`execution_log_group`]) — a group
//! API Gateway publishes under its OWN name, so it is there as soon as a configuration turning
//! the log on is accepted ([`crate::ops::update_stage`]) rather than from the first record
//! written into it — and the trace is worded exactly as `TestInvokeMethod` words the log of a
//! simulated invocation: one vocabulary, so a developer reading a stage's execution log and one
//! reading a test invocation's see the same steps.
//!
//! The trace is the steps the stage TOOK, so a request it could not serve through the
//! method's integration — a configuration this api cannot be performed through, a grant the
//! backend does not carry — carries the step that stopped it: the configuration error that
//! was found and the gateway response the request was then answered through ([`Unperformed`]).
//! A request whose integration WAS performed and whose backend answered something this api
//! cannot use — a Lambda proxy response API Gateway cannot read, answered `502` from that
//! answer itself rather than through a gateway response — carries that configuration error
//! too, in API Gateway's own words for it
//! ([`super::lambda_proxy::MALFORMED_PROXY_RESPONSE`]). A developer reads those lines to tell
//! a backend that answered badly from one that was never reached at all, and a status alone
//! does not say which.

use std::collections::BTreeMap;
use std::time::Duration;

use super::{RtRequest, RtResponse};
use smithy_server_runtime::OperationContext;

/// What serving ONE request left for what the stage REPORTS about it once it has been
/// answered: the logs this module delivers, and the metrics the stage publishes
/// ([`super::stage_metrics`]).
///
/// Filled by the serving path as it decides the request and read by both, because none of it
/// can be recovered from the response: the values an AUTHORIZATION published are not in the
/// body, a refusal's own gateway response type is not either, and neither is what the stage's
/// cache did with the request or how long its integration took. One carrier, threaded through
/// the serving once, so the two reports cannot see different facts about one request.
#[derive(Debug, Default)]
pub(super) struct Recorded {
    /// What serving the request OBSERVED for the stage's own metrics: whether its cache
    /// answered it, and how long the method's integration took
    /// ([`super::stage_metrics::Observed`]).
    pub(super) observed: super::stage_metrics::Observed,
    /// What this request's own AUTHORIZATION published about the caller — a custom
    /// authorizer's `principalId` and context values, a user pool's claims — each under the
    /// `$context` member path a reference reads it by, which a record's format may name as
    /// `$context.authorizer.<member>`; empty for a request no authorizer decided.
    pub(super) authorized: BTreeMap<String, String>,
    /// WHO the request's own authorization AUTHENTICATED, each under the `$context.identity`
    /// member path a reference reads it by ([`super::authenticated_identity`]); empty for a
    /// call API Gateway authenticated no principal for, which is every method whose
    /// `authorizationType` is not `AWS_IAM`.
    pub(super) authenticated: BTreeMap<String, String>,
    /// The configuration error the method's integration ran into, for a request the stage
    /// could not serve through it and for one whose backend answered something API Gateway
    /// could not use; `None` for every request whose integration answered usably, was
    /// served from the cache or was never reached.
    pub(super) unperformed: Option<Unperformed>,
    /// What this request EXCHANGED with the BACKEND of the method's integration, where it
    /// reached one ([`Exchanged`]); `None` for a request that reached no backend at all — a
    /// `MOCK` integration, which is answered from the api's own records, a request the
    /// stage's cache answered without performing the integration, and one no integration
    /// could be performed for.
    pub(super) exchanged: Option<Exchanged>,
}

/// The payloads one request exchanged with the backend of the method's integration, which are
/// two of the four transformation points a data trace reports ([`trace`]).
///
/// Recorded as the serving performs the integration, because neither can be recovered
/// afterwards: the method response a reader of the response holds is what the integration
/// response MAPPED, and the integration request is what the integration's own
/// `requestTemplates` and `passthroughBehavior` made of the method request.
#[derive(Debug)]
pub(super) struct Exchanged {
    /// The integration request AS SENT to the backend — the method request after the
    /// integration's own request transformation.
    pub(super) sent: Vec<u8>,
    /// The backend's own answer AS RECEIVED — before the integration response's mapping of
    /// it.
    pub(super) received: Vec<u8>,
}

/// A configuration error of the method's integration, as the execution log records it.
#[derive(Debug)]
pub(super) struct Unperformed {
    /// What the error was, in one sentence: the words API Gateway states this class of
    /// configuration error in.
    reason: String,
    /// The `GatewayResponseType` the request was answered through, which is what names the
    /// CLASS of the configuration error to a developer reading the trace; `None` for an
    /// error the request was NOT answered through a gateway response for — a backend answer
    /// API Gateway could not use is answered from that answer itself, so no gateway response
    /// type is involved and the trace claims none.
    response_type: Option<&'static str>,
}

impl Recorded {
    /// Record that `unserved` is what stopped this request's integration.
    pub(super) fn stopped_by(&mut self, unserved: &super::UnservedIntegration) {
        self.unperformed = Some(Unperformed {
            reason: unserved.reason.clone(),
            response_type: Some(unserved.refusal.response_type()),
        });
    }

    /// Record that the integration WAS performed and `reason` is the configuration error its
    /// backend's own answer was — a Lambda proxy response API Gateway cannot read, which it
    /// answers `502` from rather than through a gateway response of the api
    /// ([`super::lambda_proxy::MALFORMED_PROXY_RESPONSE`]).
    pub(super) fn answered_badly(&mut self, reason: &'static str) {
        self.unperformed = Some(Unperformed {
            reason: reason.to_string(),
            response_type: None,
        });
    }

    /// Record what this request exchanged with the backend of the method's integration: the
    /// integration request as it was SENT and the backend's answer as it was RECEIVED
    /// ([`Exchanged`]).
    pub(super) fn exchanged_with(&mut self, sent: Vec<u8>, received: Vec<u8>) {
        self.exchanged = Some(Exchanged { sent, received });
    }
}

/// The prefix API Gateway publishes every stage's execution log group under:
/// `API-Gateway-Execution-Logs_{rest-api-id}/{stage-name}`.
const EXECUTION_LOG_GROUP_PREFIX: &str = "API-Gateway-Execution-Logs_";

/// The `loggingLevel` that writes the whole trace of a request.
const LOGGING_LEVEL_INFO: &str = "INFO";

/// The `loggingLevel` that writes the trace of a request the stage answered with an error.
const LOGGING_LEVEL_ERROR: &str = "ERROR";

/// The lowest status that IS an error, which is what [`LOGGING_LEVEL_ERROR`] writes a trace
/// for: the 4xx and 5xx classes RFC 9110 defines as the client-error and server-error
/// classes.
const FIRST_ERROR_STATUS: u16 = 400;

/// A `loggingLevel` execution logging is ON at, and the CLASS one line of a trace belongs to.
///
/// `MethodSetting$loggingLevel` "specifies the logging level for this method, which affects
/// the log entries pushed to Amazon CloudWatch Logs", with "valid values are `OFF`, `ERROR`,
/// and `INFO`": the levels are nested, so `ERROR` pushes the entries that report the request's
/// FAILURE and `INFO` pushes those and the informational steps beside them. Every line of a
/// trace therefore belongs to one of the two, and the setting in force decides which of them
/// are written ([`Self::writes`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    /// Reports the failure of the request: `ERROR` writes it, and so does `INFO`.
    Error,
    /// An informational step of serving the request: only `INFO` writes it.
    Info,
}

impl Level {
    /// The level `loggingLevel` names, or `None` for a value execution logging is off at —
    /// `OFF`, no setting at all, and any other value, none of which writes a trace.
    fn of(level: Option<&str>) -> Option<Self> {
        match level {
            Some(LOGGING_LEVEL_INFO) => Some(Self::Info),
            Some(LOGGING_LEVEL_ERROR) => Some(Self::Error),
            _ => None,
        }
    }

    /// Whether a trace written at this level carries a line of the class `line`.
    fn writes(self, line: Self) -> bool {
        self == Self::Info || line == Self::Error
    }
}

/// Whether a method whose `loggingLevel` is `level` has EXECUTION LOGGING ON — the two
/// documented levels that write a trace at all, as against `OFF` and no setting, which write
/// nothing ([`traces`] decides which request each of the two writes one for).
///
/// `pub(crate)` because the stage's execution log GROUP is published for exactly this
/// configuration, which is what the control plane provisions it for
/// ([`crate::ops::update_stage`]): one reading of "logging is on", so a stage whose group
/// exists and one whose requests are traced cannot disagree.
pub(crate) fn logs_executions(level: Option<&str>) -> bool {
    Level::of(level).is_some()
}

/// The execution log group of the stage `stage` of the api `api_id`.
///
/// `pub(crate)` for the control plane, which provisions this group as soon as a stage's
/// execution logging is turned on; see [`logs_executions`].
pub(crate) fn execution_log_group(api_id: &str, stage: &str) -> String {
    format!("{EXECUTION_LOG_GROUP_PREFIX}{api_id}/{stage}")
}

/// The stream a stage's records of one UTC day are written into: that day, the stage and the
/// api, so the records of one stage of one api accumulate in one stream per day and two
/// stages pointed at ONE destination still write two streams.
///
/// The day is taken from the one timestamp rendering this service has — the date half of the
/// `date-time` form — rather than from a second calendar here.
fn log_stream_name(api_id: &str, stage: &str) -> String {
    let stamp = crate::state::now()
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_default();
    let day = stamp.split('T').next().unwrap_or_default().to_string();
    format!("{day}/[{stage}]{api_id}")
}

/// What one served request leaves in CloudWatch Logs, read out of the store in ONE pass: the
/// Region each delivery is made in, and the group, stream and records of each of the two
/// logs this stage writes.
#[derive(Debug, Default)]
struct Planned {
    /// The Region the api's own ARN names, which is where its log groups live.
    region: String,
    /// The stage's access-log destination and the record rendered for this request, when
    /// the stage carries `accessLogSettings` naming a log group and a format.
    access: Option<(String, String)>,
    /// The stage's execution log group and the trace of this request, when the method
    /// settings in force turn execution logging on.
    execution: Option<(String, Vec<String>)>,
    /// The stream both deliveries are written into ([`log_stream_name`]).
    stream: String,
}

impl Planned {
    /// Whether this request leaves anything at all, which is what decides whether the
    /// deliveries below are made.
    fn is_empty(&self) -> bool {
        self.access.is_none() && self.execution.is_none()
    }
}

/// Deliver what the stage this request reached logs about it.
///
/// Called from the ONE funnel every invoke of this data plane leaves through, with the
/// response the stage answered in hand: an access-log record is about a request that was
/// ANSWERED, so its status, its size and the latency of answering it are part of the record
/// and cannot be known before then.
///
/// `latency` is how long the whole invoke took, which is what `$context.responseLatency`
/// ("the response latency in ms") reports.
///
/// `recorded` is what serving this request left for its logs: what its own AUTHORIZATION
/// published about the caller, and the configuration error that stopped its integration where
/// one did ([`Recorded`]).
///
/// Nothing here can fail the request: the stage has already answered, so a delivery that
/// could not be made is traced by `service_common` and nothing else.
pub(super) async fn delivered<Eng: Send + Sync + 'static>(
    engine: &Eng,
    req: &RtRequest,
    ctx: &OperationContext<'_>,
    ids: &super::CallIds,
    response: &RtResponse,
    latency: Duration,
    recorded: &Recorded,
) {
    let planned = planned(engine, req, ids, response, latency, recorded);
    let Some(planned) = planned.filter(|planned| !planned.is_empty()) else {
        return;
    };
    if let Some((group, record)) = &planned.access {
        ::service_common::api_gateway_access_logs::delivered(
            ctx,
            &planned.region,
            group,
            &planned.stream,
            std::slice::from_ref(record),
        )
        .await;
    }
    if let Some((group, trace)) = &planned.execution {
        // The stage's EXECUTION log group is one API Gateway publishes under its own name
        // ([`execution_log_group`]), so this delivery provisions it; the access-log
        // destination above is a group of the account's and is never created here.
        ::service_common::cloudwatch_logs::delivered(
            ctx,
            &planned.region,
            group,
            &planned.stream,
            trace,
        )
        .await;
    }
}

/// What this request leaves behind, read under ONE shared read of the store, which is
/// RELEASED before any delivery is made: a write into another service is a call, and no lock
/// of this service's is held while another service answers.
///
/// `None` when nothing is logged at all: an account with no CloudWatch role for API Gateway
/// to write as, a request that reached no stage of an api this service holds, or a stage
/// carrying neither logging configuration.
fn planned<Eng: Send + Sync + 'static>(
    engine: &Eng,
    req: &RtRequest,
    ids: &super::CallIds,
    response: &RtResponse,
    latency: Duration,
    recorded: &Recorded,
) -> Option<Planned> {
    let state = crate::state::service_state(engine)?;
    let store = state.lock();
    // The account's CloudWatch role is what API Gateway writes as; see the module docs.
    store.account.cloudwatch_role_arn.as_ref()?;
    let resolved = super::resolve(&store, &super::Addressed::of_request(req))?;
    let api = resolved.api;
    let (stage_name, resource_path) = super::split_stage(resolved.invoke_path)?;
    let stage = api.stages.get(stage_name)?;
    // WHICH deployed method the request reached, where it reached one: the access log names
    // the resource path of the matched method, and the execution log is gated by the settings
    // in force for exactly that method. A request that matched none — a path this deployment
    // does not carry — is still a request the stage answered, so it is logged under the path
    // and verb the CLIENT addressed.
    let deployment = api.deployments.get(&stage.deployment_id);
    let matched = deployment.and_then(|deployment| {
        let path = super::match_resource_path(deployment, &resource_path)?;
        let (verb, _) = super::match_method(deployment, path, req.method())?;
        Some((path, verb))
    });
    let (matched_path, verb) = matched.unwrap_or((resource_path.as_str(), req.method()));
    // The verb the CLIENT SENT, which is what a log says about the request's HTTP method:
    // `$context.httpMethod` of the access-log format (rendered by `call_context`) and the
    // execution log's own method line. It is NOT the `verb` above, which names the deployed
    // METHOD that served the request — the `ANY` catch-all for a verb the resource carries no
    // method of its own for, which is a configured name rather than a verb any request
    // carries.
    let request_verb = crate::ops::put_method::wire_verb(req.method());
    let status = response.status().as_u16();
    let mut context = super::call_context(
        &api.id,
        stage_name,
        matched_path,
        // What the CLIENT addressed within the api, which `$context.path` reports
        // stage-qualified: a parameterised resource's template is `matched_path`, and the
        // path asked for is this.
        &resource_path,
        req,
        ids,
        ::std::time::SystemTime::now(),
    );
    // What the request's own AUTHORIZATION published, under the same `$context` member paths
    // the call's mapping templates read it by: `$context.authorizer.principalId` is "the
    // principal user identification associated with the token sent by the client and returned
    // from an API Gateway Lambda authorizer function", which is an access-log variable of its
    // own, and a user pool's claims are read as `$context.authorizer.claims.<name>`. A
    // request no authorizer decided publishes nothing, so such a reference stays unresolved.
    context.extend(
        recorded
            .authorized
            .iter()
            .map(|(member, value)| (member.clone(), value.clone())),
    );
    // WHO the request's authorization authenticated, under the `$context.identity` member
    // paths the address and the user agent of the call sit under: `$context.identity.caller`
    // and the members beside it are access-log variables, and the values they report are the
    // ones the serving resolved — never a second reading of the caller taken after the fact.
    context.extend(
        recorded
            .authenticated
            .iter()
            .map(|(member, value)| (member.clone(), value.clone())),
    );
    answered_context(&mut context, response, latency);

    let access = stage
        .access_log_settings
        .as_ref()
        .and_then(|settings| {
            let destination = settings.destination_arn.as_deref()?;
            let format = settings.format.as_deref()?;
            let group =
                ::service_common::api_gateway_access_logs::destination_log_group(destination)?;
            Some((
                group.to_string(),
                ::service_common::api_gateway_access_logs::rendered(format, &context),
            ))
        });

    let execution = super::response_cache::method_settings_in_force(stage, matched_path, verb)
        .and_then(|settings| {
            let level = Level::of(settings.logging_level.as_deref())?;
            traces(level, status).then_some((level, settings))
        })
        .map(|(level, settings)| {
            (
                execution_log_group(&api.id, stage_name),
                trace(
                    req,
                    response,
                    &ids.request_id,
                    &request_verb,
                    matched_path,
                    deployment
                        .and_then(|deployment| {
                            deployment
                                .resources
                                .values()
                                .find(|resource| resource.path == matched_path)
                        })
                        .and_then(|resource| resource.methods.get(verb))
                        .and_then(|method| method.integration.as_ref())
                        .and_then(|integration| integration.r#type.as_deref()),
                    recorded.unperformed.as_ref(),
                    recorded.exchanged.as_ref(),
                    level,
                    settings.data_trace_enabled,
                ),
            )
        });

    Some(Planned {
        region: api.arn_region().to_string(),
        access,
        execution,
        stream: log_stream_name(&api.id, stage_name),
    })
}

/// Whether a method whose execution logging is on at `level` writes a trace of a request
/// answered with `status`; see the module docs.
fn traces(level: Level, status: u16) -> bool {
    match level {
        Level::Info => true,
        Level::Error => status >= FIRST_ERROR_STATUS,
    }
}

/// The `$context` values of the ANSWER, added beside the values of the call: what the stage
/// answered with, how large that answer was and how long answering took.
///
/// `$context.error.message` is the message a REFUSED call was answered with — every refusal
/// this data plane renders is a gateway response whose body carries a `message` member — so
/// it is read off the answer for a status that IS an error and left unset otherwise, where
/// there is no error to name. `$context.error.messageString` is "the quoted value of
/// `$context.error.message`", so it is that same text quoted.
fn answered_context(
    context: &mut BTreeMap<String, String>,
    response: &RtResponse,
    latency: Duration,
) {
    let status = response.status().as_u16();
    let body = response.body().bytes().unwrap_or_default();
    context.insert("status".to_string(), status.to_string());
    context.insert("responseLength".to_string(), body.len().to_string());
    context.insert(
        "responseLatency".to_string(),
        latency.as_millis().to_string(),
    );
    if status < FIRST_ERROR_STATUS {
        return;
    }
    let message = ::serde_json::from_slice::<::serde_json::Value>(body)
        .ok()
        .as_ref()
        .and_then(|document| document.get("message"))
        .and_then(|message| message.as_str().map(str::to_string));
    if let Some(message) = message {
        context.insert(
            "error.messageString".to_string(),
            ::serde_json::Value::String(message.clone()).to_string(),
        );
        context.insert("error.message".to_string(), message);
    }
}

/// The execution log of one request: the steps the stage took to answer it, in the order it
/// took them, worded as `TestInvokeMethod` words the log of a simulated invocation.
///
/// One log EVENT per step, because that is how CloudWatch Logs holds a multi-line trace: a
/// client filtering the group for one step finds the event that step is. Every one of them
/// NAMES the request it is about, in the form API Gateway prefixes a record of a stage's
/// execution log with — the request's own id in parentheses ([`about_request`]) — because one
/// group holds the records of every request the stage served and a client reading it has
/// nothing else to separate two traces by.
///
/// `level` is the `loggingLevel` in force, which decides WHICH of those lines are written:
/// every line belongs to one of the two documented levels ([`Level`]), so a trace written at
/// `ERROR` carries the lines that report the failure and none of the informational steps.
///
/// `data_trace` is what puts the payloads of the call in the trace: with it off the trace
/// carries no payload line at all, and with it on each payload line carries the bytes
/// ([`payload_line`]). Both payload lines are informational, so what a data trace adds is
/// something a trace written at `INFO` carries.
///
/// `exchanged` is what the request exchanged with the BACKEND of the method's integration
/// ([`Exchanged`]), which is where the two inner transformation points of the data trace come
/// from: a request that reached no backend has neither line, because nothing crossed either
/// point.
///
/// `unperformed` is the configuration error of the method's integration, for a request the
/// stage could not serve through it or whose backend answered something this api could not
/// use: it is recorded where the integration's own step belongs — after the type that was to
/// answer and before what the caller was answered with — in the sentences a simulated
/// invocation's log words that outcome with.
#[allow(clippy::too_many_arguments)]
fn trace(
    req: &RtRequest,
    response: &RtResponse,
    request_id: &str,
    // The verb the CLIENT SENT, which is the request's own HTTP method; see the caller.
    verb: &str,
    resource_path: &str,
    integration_type: Option<&str>,
    unperformed: Option<&Unperformed>,
    exchanged: Option<&Exchanged>,
    level: Level,
    data_trace: bool,
) -> Vec<String> {
    let request_body = req.body().bytes().unwrap_or_default();
    let response_body = response.body().bytes().unwrap_or_default();
    let mut lines = vec![
        // The line that OPENS a request's log, which is the one line of it that is about the
        // request rather than about a step of serving it.
        (Level::Error, format!("Execution log for request {request_id}")),
        (
            Level::Info,
            format!("Starting execution for request: {request_id}"),
        ),
        (
            Level::Info,
            format!("HTTP Method: {verb}, Resource Path: {resource_path}"),
        ),
        (Level::Info, traced_header_names_line(&header_names(req))),
    ];
    // The REQUEST's payload, which only a data trace puts in the log ([`payload_line`]).
    lines.extend(
        payload_line(
            "Method request body before transformations",
            request_body,
            data_trace,
        )
        .map(|line| (Level::Info, line)),
    );
    lines.push((
        Level::Info,
        format!("Integration type: {}", integration_type.unwrap_or("none")),
    ));
    // The integration REQUEST as it was sent to the backend, which only a data trace puts in
    // the log: a request that reached no backend carries no such line, because nothing
    // crossed that point.
    lines.extend(
        exchanged
            .and_then(|exchanged| {
                payload_line(
                    "Endpoint request body after transformations",
                    &exchanged.sent,
                    data_trace,
                )
            })
            .map(|line| (Level::Info, line)),
    );
    if let Some(unperformed) = unperformed {
        lines.push((
            Level::Error,
            format!(
                "Execution failed due to configuration error: {}",
                unperformed.reason
            ),
        ));
        if let Some(response_type) = unperformed.response_type {
            lines.push((
                Level::Error,
                format!("Answered through this API's {response_type} gateway response"),
            ));
        }
    }
    // The BACKEND's own answer as it arrived, before this api's own responses mapped it, and
    // then the RESPONSE's payload, both gated by the same data trace as the request's.
    lines.extend(
        exchanged
            .and_then(|exchanged| {
                payload_line(
                    "Endpoint response body before transformations",
                    &exchanged.received,
                    data_trace,
                )
            })
            .map(|line| (Level::Info, line)),
    );
    lines.extend(
        payload_line(
            "Method response body after transformations",
            response_body,
            data_trace,
        )
        .map(|line| (Level::Info, line)),
    );
    lines.push((
        Level::Info,
        format!(
            "Method completed with status: {}",
            response.status().as_u16()
        ),
    ));
    lines
        .into_iter()
        .filter(|(written_at, _)| level.writes(*written_at))
        .map(|(_, line)| about_request(request_id, &line))
        .collect()
}

/// One record of a stage's execution log, naming the request it is about: API Gateway writes
/// each line of a request's trace prefixed with that request's own id in parentheses, which is
/// the id the response reported in its own header — so a client holding a response can find
/// exactly the records of the request it holds in a group that carries every request's.
fn about_request(request_id: &str, line: &str) -> String {
    format!("({request_id}) {line}")
}

/// Emulator resource guard, NOT an AWS limit: the most bytes of one payload a traced line
/// carries.
///
/// A payload's size is the CLIENT's choice, and a log event is itself bounded — CloudWatch
/// Logs takes at most one megabyte per event — so a payload past this is reported by its SIZE
/// (the line the trace writes when data tracing is off) rather than taking the whole trace
/// down with it. The figure is generous enough that no payload a method request carries in
/// practice is reported by its size instead of its bytes.
const MAX_TRACED_PAYLOAD_BYTES: usize = 8 * 1024;

/// Emulator resource guard, NOT an AWS limit: the most header NAMES one traced line carries.
///
/// How many headers a request carries is the CALLER's choice, and a log event is itself
/// bounded — CloudWatch Logs takes at most one megabyte per event — so the names past this are
/// reported by their COUNT rather than growing one line with the request, which is the reading
/// [`MAX_TRACED_PAYLOAD_BYTES`] takes of a payload. The figure is generous enough that no
/// request a client sends in practice has a name of its left out.
pub(crate) const MAX_TRACED_HEADER_NAMES: usize = 128;

/// The `Method request headers` line of an execution log: the names the request carries, in
/// the order they are given, within [`MAX_TRACED_HEADER_NAMES`], with any past that reported
/// by their count.
///
/// One copy, shared with the two `TestInvoke*` operations: the trace of a request a STAGE
/// served and the log of a simulated invocation word this line the same way, so they are also
/// bounded the same way, and the bound is applied where the line is built rather than left to
/// each caller.
pub(crate) fn traced_header_names_line(names: &[String]) -> String {
    let listed = names
        .iter()
        .take(MAX_TRACED_HEADER_NAMES)
        .map(String::as_str)
        .collect::<Vec<&str>>()
        .join(", ");
    match names.len().saturating_sub(MAX_TRACED_HEADER_NAMES) {
        0 => format!("Method request headers: [{listed}]"),
        past => format!("Method request headers: [{listed}] and {past} more"),
    }
}

/// One payload line of the trace, or `None` where the trace carries none.
///
/// `dataTrace` is what puts a call's payloads in the log — "specifies whether data trace
/// logging is enabled for this method, which affects the log entries pushed to Amazon
/// CloudWatch Logs" (`MethodSetting$dataTrace`) — so the payload lines ARE the data trace: with
/// it off the trace carries no line about a payload at all, and with it on the line carries the
/// payload's bytes. A payload past [`MAX_TRACED_PAYLOAD_BYTES`] is reported by its size
/// instead, which is this emulator's resource guard on one log event rather than a line API
/// Gateway words that way.
fn payload_line(what: &str, payload: &[u8], data_trace: bool) -> Option<String> {
    if !data_trace {
        return None;
    }
    if payload.len() <= MAX_TRACED_PAYLOAD_BYTES {
        return Some(format!("{what}: {}", String::from_utf8_lossy(payload)));
    }
    Some(format!("{what}: {} byte(s)", payload.len()))
}

/// The NAMES of the headers the request carries, in name order.
///
/// The names, not the values: a header is where a caller's authorization token or API key
/// travels, and an execution log is not where a caller's credential belongs — the same
/// reading `TestInvokeMethod`'s own log takes.
fn header_names(req: &RtRequest) -> Vec<String> {
    let mut names: Vec<String> = req
        .headers()
        .iter()
        .map(|(name, _)| name.to_string())
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_execution_group_is_the_one_api_gateway_publishes_for_the_stage() {
        for (case, api, stage, expected) in [
            (
                "a stage of an api",
                "abc123",
                "prod",
                "API-Gateway-Execution-Logs_abc123/prod",
            ),
            (
                "another stage of the same api",
                "abc123",
                "dev",
                "API-Gateway-Execution-Logs_abc123/dev",
            ),
        ] {
            let got = execution_log_group(api, stage);
            assert_eq!(
                got, expected,
                "case {case:?}: expected {expected:?}, got {got:?}"
            );
        }
    }

    #[test]
    fn the_logging_level_in_force_decides_which_requests_leave_a_trace() {
        for (case, level, status, expected) in [
            ("INFO logs a served request", Some("INFO"), 200, true),
            ("INFO logs a refused request", Some("INFO"), 403, true),
            ("ERROR logs no served request", Some("ERROR"), 200, false),
            ("ERROR logs a refused request", Some("ERROR"), 500, true),
            ("OFF logs nothing", Some("OFF"), 500, false),
            ("no level logs nothing", None, 500, false),
        ] {
            let got = Level::of(level).is_some_and(|level| traces(level, status));
            assert_eq!(
                got, expected,
                "case {case:?}: expected {expected}, got {got}"
            );
        }
    }

    #[test]
    fn the_logging_level_in_force_decides_which_lines_a_trace_carries() {
        for (case, level, line, expected) in [
            ("INFO writes a failure line", Level::Info, Level::Error, true),
            (
                "INFO writes an informational line",
                Level::Info,
                Level::Info,
                true,
            ),
            (
                "ERROR writes a failure line",
                Level::Error,
                Level::Error,
                true,
            ),
            (
                "ERROR writes no informational line",
                Level::Error,
                Level::Info,
                false,
            ),
        ] {
            let got = level.writes(line);
            assert_eq!(
                got, expected,
                "case {case:?}: expected {expected}, got {got}"
            );
        }
    }

    #[test]
    fn a_payload_line_is_the_data_trace_and_carries_its_bytes_within_the_guard() {
        let label = "Method request body before transformations";
        let at_the_bound = "x".repeat(MAX_TRACED_PAYLOAD_BYTES);
        let past_the_bound = "x".repeat(MAX_TRACED_PAYLOAD_BYTES + 1);
        for (case, payload, data_trace, expected) in [
            (
                "a small payload, traced",
                r#"{"id":"1"}"#,
                true,
                Some(format!("{label}: {}", r#"{"id":"1"}"#)),
            ),
            (
                "a small payload with the data trace off, which logs no payload line",
                r#"{"id":"1"}"#,
                false,
                None,
            ),
            (
                "a payload with the data trace off, whatever its size",
                past_the_bound.as_str(),
                false,
                None,
            ),
            (
                "no payload at all, traced",
                "",
                true,
                Some(format!("{label}: ")),
            ),
            (
                "a payload at the guard, traced",
                at_the_bound.as_str(),
                true,
                Some(format!("{label}: {at_the_bound}")),
            ),
            (
                "a payload past the guard, traced",
                past_the_bound.as_str(),
                true,
                Some(format!("{label}: {} byte(s)", MAX_TRACED_PAYLOAD_BYTES + 1)),
            ),
        ] {
            let got = payload_line(label, payload.as_bytes(), data_trace);
            assert_eq!(
                got, expected,
                "case {case:?}: expected {expected:?}, got {got:?}"
            );
        }
    }

    #[test]
    fn a_traced_header_line_carries_the_names_within_the_guard_and_counts_the_rest() {
        let names = |count: usize| -> Vec<String> {
            (0..count).map(|index| format!("h{index}")).collect()
        };
        let joined = |count: usize| names(count).join(", ");
        for (case, carried, expected) in [
            ("no header at all", 0, "Method request headers: []".to_string()),
            (
                "one header",
                1,
                "Method request headers: [h0]".to_string(),
            ),
            (
                "a handful of headers",
                3,
                format!("Method request headers: [{}]", joined(3)),
            ),
            (
                "as many headers as the guard carries",
                MAX_TRACED_HEADER_NAMES,
                format!(
                    "Method request headers: [{}]",
                    joined(MAX_TRACED_HEADER_NAMES)
                ),
            ),
            (
                "one header past the guard",
                MAX_TRACED_HEADER_NAMES + 1,
                format!(
                    "Method request headers: [{}] and 1 more",
                    joined(MAX_TRACED_HEADER_NAMES)
                ),
            ),
            (
                "far more headers than the guard carries",
                MAX_TRACED_HEADER_NAMES * 4,
                format!(
                    "Method request headers: [{}] and {} more",
                    joined(MAX_TRACED_HEADER_NAMES),
                    MAX_TRACED_HEADER_NAMES * 3
                ),
            ),
        ] {
            let got = traced_header_names_line(&names(carried));
            assert_eq!(
                got, expected,
                "case {case:?}: expected {expected:?}, got {got:?}"
            );
        }
    }
}
