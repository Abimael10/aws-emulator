// SPDX-License-Identifier: CC0-1.0

//! The Apache Airflow CLI endpoint of an environment's web server: `POST
//! <WebServerHostname>/aws_mwaa/cli`.
//!
//! # Why this endpoint exists
//!
//! `CreateCliToken` mints "an Airflow CLI login token" and reports the web server's address
//! beside it, and the ONLY thing that pair is for is this endpoint: a client posts an Apache
//! Airflow CLI command to it, carrying the token as `Authorization: Bearer <CliToken>`, and
//! reads the command's output out of the JSON document it answers with
//! ([`STDOUT_MEMBER`] / [`STDERR_MEMBER`], each base64-encoded). It is the web SERVER's own
//! endpoint rather than an AWS API operation, so it is in no Smithy model, names no
//! operation and carries no SigV4 signature — which is exactly why a request to it resolved
//! to no service at all and was answered `404 UnknownService` by the emulator's front door,
//! for an address this service had just told the client to use.
//!
//! # How it is served
//!
//! Like every other data plane of this tree, it is claimed by the service itself: the
//! request addresses the address an environment publishes ([`crate::state::webserver_url`]),
//! which is the endpoint this emulator serves this service's whole surface at, and carries
//! [`CLI_PATH`] — a path no modelled operation of this service is bound under. The service
//! claims it in `can_handle` ([`claims_parts`]) and serves it as ONE operation of its own
//! ([`AirflowCliEndpoint`], resolved by `override_operation`), which is what gives the
//! endpoint its own authentication: the credential is the bearer token this service minted,
//! so there is no signature for the framework to verify.
//!
//! The token is the whole of what identifies the environment — the published address is the
//! same for every environment of one emulator, because one web server address is one
//! emulator endpoint — so the environment a command runs against is the one that MINTED the
//! presented token ([`crate::state::ComAmazonawsMwaaAmazonmwaaState::environment_for_cli_token`]).
//! A request presenting no bearer credential is answered `401` with the challenge, and one
//! presenting a token no stored environment minted `403`: neither reaches a command.
//!
//! The command itself is read and answered by [`crate::state::airflow_cli`], against that
//! environment's own stored Apache Airflow metadata.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use aws_smithy_types::body::SdkBody;
use smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};

/// The path an environment's Apache Airflow web server serves the CLI endpoint at, as the
/// Amazon MWAA documentation and every CLI wrapper spell it.
pub const CLI_PATH: &str = "/aws_mwaa/cli";

/// The method the endpoint serves: the command travels in the request BODY.
const POST: &str = "POST";

/// The member the answer carries the command's standard output in, base64-encoded.
pub const STDOUT_MEMBER: &str = "stdout";

/// The member it carries standard error in, base64-encoded — present only when the command
/// wrote to it.
pub const STDERR_MEMBER: &str = "stderr";

/// The media type the answer is a document of.
const JSON_MEDIA_TYPE: &str = "application/json";

/// The media type a client sends an Apache Airflow CLI command as.
pub const COMMAND_MEDIA_TYPE: &str = "text/plain";

/// The authentication scheme the token is presented under (RFC 6750).
const BEARER_SCHEME: &str = "Bearer";

/// The longest command line this endpoint reads, in bytes.
///
/// An Apache Airflow CLI command is a line of text; the ceiling is the emulator's own, and
/// is what keeps the bytes one request materialises from being the caller's choice of size.
const MAX_COMMAND_BYTES: usize = 8 * 1024;

/// Whether this endpoint serves the request these `parts` came from — read by
/// `Service::can_handle`, so a request addressed to the web server address an environment
/// publishes reaches this service instead of the front door's `UnknownService`.
///
/// Claimed by the PATH alone, for every method: the path is this endpoint's own (no modelled
/// operation of this service is bound under it, and no other service serves it), and a
/// request that names it with another method is this endpoint's to refuse rather than
/// another claimant's to mis-serve.
pub fn claims_parts(parts: &::http::request::Parts) -> bool {
    serves_path(parts.uri.path())
}

/// [`claims_parts`] for the runtime-API request the serving seam is handed, read by the same
/// rule.
pub fn claims_request(req: &RtRequest) -> bool {
    serves_path(&request_path(req.uri()))
}

/// Whether `path` addresses this endpoint: [`CLI_PATH`], with or without a trailing `/`.
fn serves_path(path: &str) -> bool {
    path.trim_end_matches('/') == CLI_PATH
}

/// The path of a request target that may be absolute (`https://host/path`) or origin-form
/// (`/path`), read with the `http` crate's own parse — the same reading the rest of this
/// service's routing applies to a URI. A target that parse does not accept is read by
/// cutting its query and fragment off, so such a request is still routed by its path.
fn request_path(uri: &str) -> String {
    if let Ok(parsed) = uri.parse::<::http::Uri>() {
        return parsed.path().to_string();
    }
    let without_fragment = uri.split_once('#').map_or(uri, |(before, _)| before);
    without_fragment
        .split_once('?')
        .map_or(without_fragment, |(path, _)| path)
        .to_string()
}

/// The bearer token an `Authorization` header presents, or `None` when it carries no
/// non-empty `Bearer` credential.
fn presented_token(authorization: Option<&str>) -> Option<&str> {
    let value = authorization?.trim();
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case(BEARER_SCHEME) {
        return None;
    }
    let token = token.trim();
    (!token.is_empty()).then_some(token)
}

/// One answer of this endpoint: a status and a JSON document.
struct Answer {
    status: u16,
    body: Vec<u8>,
    /// The authentication challenge a refusal of a missing credential states.
    challenge: bool,
}

impl Answer {
    /// The document a command's output is reported in: `stdout` and `stderr`, each
    /// base64-encoded, and a stream the command wrote nothing to left out — there is no
    /// output to report for it.
    fn output(output: &crate::state::airflow_cli::CliOutput) -> Self {
        let mut document = ::serde_json::Map::new();
        document.insert(
            STDOUT_MEMBER.to_string(),
            ::serde_json::Value::String(encoded(&output.stdout)),
        );
        if !output.stderr.is_empty() {
            document.insert(
                STDERR_MEMBER.to_string(),
                ::serde_json::Value::String(encoded(&output.stderr)),
            );
        }
        Self {
            status: 200,
            body: ::serde_json::Value::Object(document).to_string().into_bytes(),
            challenge: false,
        }
    }

    /// One refusal, in the `{"message": …}` envelope a web server states one in.
    fn refused(status: u16, message: &str) -> Self {
        Self {
            status,
            body: ::serde_json::json!({ "message": message })
                .to_string()
                .into_bytes(),
            challenge: false,
        }
    }

    /// A refusal of a request presenting no credential, which states the scheme one is
    /// presented under.
    fn unauthenticated(message: &str) -> Self {
        Self {
            challenge: true,
            ..Self::refused(401, message)
        }
    }
}

/// One output stream, as the answer carries it: base64, which is what makes a document
/// carry output of any encoding unchanged.
fn encoded(stream: &str) -> String {
    ::base64::Engine::encode(
        &::base64::engine::general_purpose::STANDARD,
        stream.as_bytes(),
    )
}

/// Answer one request to this endpoint out of the environment that minted the token it
/// presents.
fn serve<Eng: Send + Sync + 'static>(engine: &Eng, req: &RtRequest) -> Answer {
    let method = req.method().to_ascii_uppercase();
    if method != POST {
        return Answer::refused(
            405,
            &format!("{CLI_PATH} is served for {POST} requests, not {method}"),
        );
    }
    let body = req.body().bytes().unwrap_or_default();
    if body.len() > MAX_COMMAND_BYTES {
        return Answer::refused(
            413,
            &format!(
                "an Apache Airflow CLI command may be at most {MAX_COMMAND_BYTES} bytes long"
            ),
        );
    }
    let Some(presented) = presented_token(req.headers().get("authorization")) else {
        return Answer::unauthenticated(
            "an Apache Airflow CLI command is authenticated by the token CreateCliToken \
             mints, presented as a bearer credential",
        );
    };
    let Some(state) = crate::state::service_state(engine) else {
        return Answer::refused(503, "the Amazon MWAA environment store is unavailable");
    };
    let now = ::std::time::SystemTime::now();
    let mut store = state.lock();
    // The transition the clock already owes, materialised before anything is read — the
    // same first step every operation of this service takes.
    store.settle(now);
    let Some(environment) = store.environment_for_cli_token(presented) else {
        // The token authenticates to no environment of this emulator: one that was never
        // minted here, one minted for an environment that has since been torn down, or one
        // that has made room for newer tokens.
        return Answer::refused(
            403,
            "the presented token is not an Apache Airflow CLI token of an environment of \
             this emulator",
        );
    };
    if !environment.webserver_is_running() {
        let status = environment.status.clone();
        return Answer::refused(
            503,
            &format!(
                "the Apache Airflow web server of an environment with {status} status is not \
                 serving"
            ),
        );
    }
    // The command is read as text, which is the media type a client sends it as; bytes that
    // are not text are no command line, and the CLI's own parser is what says so.
    //
    // It runs under the store guard, because it reads and writes that environment's own
    // metadata — and what it may do there is bounded before the guard is taken: the command
    // line is at most [`MAX_COMMAND_BYTES`], and every collection it can walk or grow is
    // held to the ceilings the metadata store already states
    // (`crate::state::MAX_AIRFLOW_VARIABLES`, `MAX_AIRFLOW_POOLS` and the key and value
    // lengths beside them), so one caller's command cannot hold this store for unbounded
    // work.
    let command = String::from_utf8_lossy(body);
    Answer::output(&crate::state::airflow_cli::run(environment, &command))
}

/// The ONE operation this service serves the Apache Airflow CLI endpoint with.
///
/// It holds the SAME shared engine every modelled operation of this service holds, so a
/// command runs against the environment the control plane created and a change it makes is
/// read back through `GetEnvironment` and `InvokeRestApi`.
pub struct AirflowCliEndpoint<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> AirflowCliEndpoint<Eng> {
    /// Build the endpoint's operation from the shared engine.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: Send + Sync + 'static> ErasedOperation for AirflowCliEndpoint<Eng> {
    fn run<'r>(
        &'r self,
        _svc: &'r dyn Service,
        req: &'r RtRequest,
        _ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move { rendered(serve(&*self.engine, req)) })
    }

    /// Served UNSIGNED: a client of an environment's Apache Airflow web server presents the
    /// CLI token this service minted, not a SigV4 signature, so there is no signature for
    /// the framework to verify. The credential is not skipped — [`serve`] resolves the
    /// presented token against the tokens the environments of this emulator minted, and
    /// answers `401`/`403` for anything else.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        Some(&[])
    }
}

/// One [`Answer`] on the wire.
fn rendered(answer: Answer) -> RtResponse {
    let status = StatusCode::try_from(answer.status)
        .unwrap_or_else(|_| StatusCode::try_from(500).expect("500 is a status"));
    let mut response = RtResponse::new(status, SdkBody::from(answer.body));
    if let Err(refused) = response
        .headers_mut()
        .try_insert("content-type", JSON_MEDIA_TYPE)
    {
        ::tracing::warn!(
            error = %refused,
            "the media type cannot be sent on the wire and is left off the response"
        );
    }
    if answer.challenge {
        if let Err(refused) = response
            .headers_mut()
            .try_insert("www-authenticate", BEARER_SCHEME)
        {
            ::tracing::warn!(
                error = %refused,
                "the authentication challenge cannot be sent on the wire and is left off the \
                 response"
            );
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the endpoint is addressed by its own path, whichever form the request target
    /// takes — and nothing else is claimed.
    #[test]
    fn only_the_cli_path_is_claimed() {
        for (case, path, claimed) in [
            ("the endpoint itself", CLI_PATH, true),
            ("a trailing slash", "/aws_mwaa/cli/", true),
            ("an absolute target", "https://127.0.0.1:9297/aws_mwaa/cli", true),
            ("a target with a query", "/aws_mwaa/cli?x=1", true),
            ("the web login endpoint", "/aws_mwaa/login", false),
            ("a modelled control-plane route", "/environments/MyEnv", false),
            ("a modelled token route", "/clitoken/MyEnv", false),
            ("a path merely containing it", "/x/aws_mwaa/cli", false),
            ("the root", "/", false),
        ] {
            let read = serves_path(&request_path(path));
            assert_eq!(
                read, claimed,
                "case {case:?}: expected the path {path:?} to be claimed == {claimed}, got \
                 {read}"
            );
        }
    }

    /// RULE: the credential is the bearer token, read out of the `Authorization` header in
    /// the forms RFC 6750 admits — and nothing else is a presented token.
    #[test]
    fn only_a_bearer_credential_presents_a_token() {
        for (case, header, expected) in [
            ("the scheme as RFC 6750 spells it", Some("Bearer a-token"), Some("a-token")),
            ("another case of the scheme", Some("bearer a-token"), Some("a-token")),
            ("whitespace around the value", Some("  Bearer   a-token  "), Some("a-token")),
            ("the scheme with no token", Some("Bearer"), None),
            ("the scheme with an empty token", Some("Bearer "), None),
            ("a SigV4 signature", Some("AWS4-HMAC-SHA256 Credential=AKID/x"), None),
            ("basic credentials", Some("Basic dXNlcjpwYXNz"), None),
            ("no header at all", None, None),
        ] {
            let read = presented_token(header);
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    /// RULE: the answer carries each stream the command wrote, base64-encoded, and leaves
    /// out a stream it wrote nothing to.
    #[test]
    fn the_answer_carries_each_stream_the_command_wrote() {
        let answered = Answer::output(&crate::state::airflow_cli::CliOutput {
            stdout: "No data found\n".to_string(),
            stderr: String::new(),
        });
        let document: ::serde_json::Value =
            ::serde_json::from_slice(&answered.body).expect("a JSON document");
        assert_eq!(
            document.get(STDOUT_MEMBER).and_then(::serde_json::Value::as_str),
            Some(encoded("No data found\n").as_str()),
            "expected the base64 of the output, got {document}"
        );
        assert!(
            document.get(STDERR_MEMBER).is_none(),
            "a stream the command wrote nothing to is left out, got {document}"
        );

        let answered = Answer::output(&crate::state::airflow_cli::CliOutput {
            stdout: String::new(),
            stderr: "airflow: error: no\n".to_string(),
        });
        let document: ::serde_json::Value =
            ::serde_json::from_slice(&answered.body).expect("a JSON document");
        assert_eq!(
            document.get(STDERR_MEMBER).and_then(::serde_json::Value::as_str),
            Some(encoded("airflow: error: no\n").as_str()),
            "expected the base64 of standard error, got {document}"
        );
        assert_eq!(
            answered.status, 200,
            "a command that failed is still a served request, got {}",
            answered.status
        );
    }
}
