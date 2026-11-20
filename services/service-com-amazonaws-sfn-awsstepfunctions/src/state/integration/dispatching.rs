// SPDX-License-Identifier: CC0-1.0

//! The in-process CALL an integration is made as: the signed request handed to the
//! service that owns the operation, and the credential it is signed with.
//!
//! One concern in a file of its own, reached from [`super`]: a task's work is a request
//! to another emulated service, and this is where that request is signed, dispatched
//! and read back as an answer.

use super::*;

/// Sign one request as the identity `signing` names — a session of the state machine's
/// execution role, or this emulator's own credential where there is none — and hand it
/// to the service its credential scope names, in process.
///
/// It is signed because the emulator verifies every request signature; the
/// credential scope is also what routes it, so the service that owns the operation
/// is the one that serves the call. The answer is its status, the error type it
/// named in the header, and its body.
pub(super) async fn dispatch(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    api: &Api,
    region: &str,
    built: Wired,
    signing: Option<&crate::state::execution_role_session::RoleSession>,
) -> Result<Answered, String> {
    let mut builder = ::http::Request::builder()
        .method(built.method)
        .uri(&built.uri)
        .header("host", api.host(region));
    // A media type describes a BODY: a request that carries none — a REST read bound
    // to its path and its query alone — names none either. What type it is, is the
    // body's own where the binding says so ([`Wired::media_type`]) and this service's
    // protocol's otherwise.
    if !built.body.is_empty() {
        let media_type = built
            .media_type
            .map_or_else(|| api.content_type(), str::to_string);
        builder = builder.header("content-type", media_type);
    }
    if let Some(target) = &built.target {
        builder = builder.header(TARGET_HEADER, target);
    }
    // A query-compatible service names a refusal by its legacy query code as well as
    // by its shape, and only for a client that asks for it.
    if api.query_compatible {
        builder = builder.header(QUERY_MODE_HEADER, "true");
    }
    for (field, value) in &built.headers {
        builder = builder.header(*field, value);
    }
    let mut request = builder
        .body(built.body)
        .map_err(|error| error.to_string())?;

    // WHO the call is made as: the session of the execution role where the walk was
    // given one, and this emulator's own credential otherwise.
    let signed_as = match signing {
        Some(session) => Signer {
            access_key_id: session.access_key_id.clone(),
            secret_access_key: session.secret_access_key.clone(),
            session_token: session.session_token.clone(),
        },
        None => {
            let (access_key_id, secret_access_key) = credentials(callback).await?;
            Signer {
                access_key_id,
                secret_access_key,
                session_token: None,
            }
        }
    };
    sign(&mut request, api.signing_name, region, &signed_as)?;

    let (parts, body) = request.into_parts();
    let request = ::http::Request::from_parts(
        parts,
        ::aws_smithy_types::body::SdkBody::from(::bytes::Bytes::from(body)),
    );
    let request = ::aws_smithy_runtime_api::http::Request::try_from(request)
        .map_err(|error| error.to_string())?;
    let response = ::aws_smithy_runtime_api::client::http::HttpConnector::call(
        &callback.http_connector(),
        request,
    )
    .await
    .map_err(|error| error.to_string())?;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get(ERROR_TYPE_HEADER)
        .map(str::to_string);
    // Every header the answer carried, in the order it carried them: the task's own
    // result reports them for an integration whose result is the literal SDK response
    // ([`with_sdk_envelope`]), so they are read off the answer rather than invented.
    let headers: Vec<(String, String)> = response
        .headers()
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    let payload = response
        .body()
        .bytes()
        .ok_or("the in-process answer carried a streaming body")?
        .to_vec();
    Ok(Answered {
        status,
        error_type,
        headers,
        payload,
    })
}

/// Make one integration call and answer the RAW answer the invoked service gave: the
/// status, the error shape it named and its body.
///
/// The seam for a caller that reports what a call came to in ITS OWN vocabulary rather
/// than as a `Task` state's result: Amazon EventBridge Scheduler's universal target
/// reports a failed invocation by the error code and the message the answer carries and
/// decides from the status whether the invocation is retried
/// ([`Target$RetryPolicy`](https://docs.aws.amazon.com/scheduler/latest/APIReference/API_RetryPolicy.html)),
/// which the ASL error names [`call_api`] reads an answer into do not carry. The request
/// is built and signed exactly as [`call_api`] builds and signs it, so both callers make
/// the same call.
pub async fn call_answered(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    call: &Call,
    request: &Value,
) -> Result<Answered, String> {
    let built = wired(call, request)?;
    // Addressed exactly as [`call_api`] addresses it: the endpoint the request's own
    // target names, where that target carries one.
    let addressed = addressed_region(call, request);
    let region = addressed.as_deref().unwrap_or(region);
    dispatch(callback, call.api, region, built, None).await
}

/// What one in-process call came back with.
pub struct Answered {
    /// The HTTP status the service answered.
    pub(super) status: u16,
    /// The error shape it named in [`ERROR_TYPE_HEADER`], for a refusal.
    pub(super) error_type: Option<String>,
    /// Every header of the answer, as it carried them.
    pub(super) headers: Vec<(String, String)>,
    /// Its body.
    pub(super) payload: Vec<u8>,
}

impl Answered {
    /// The HTTP status the invoked service answered.
    #[must_use]
    pub fn status(&self) -> u16 {
        self.status
    }

    /// The error shape it named in [`ERROR_TYPE_HEADER`]; `None` for an answer that
    /// named none.
    #[must_use]
    pub fn error_type(&self) -> Option<&str> {
        self.error_type.as_deref()
    }

    /// Its body, as it answered it.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// The first value the answer carried `name` under, compared without regard to
    /// case as HTTP field names are, through this crate's one reading of a carried
    /// header ([`carried_header`]).
    pub(super) fn header(&self, name: &str) -> Option<String> {
        carried_header(&self.headers, name).map(str::to_string)
    }

    /// The LEGACY query code a query-compatible service named this refusal's class by,
    /// out of [`QUERY_ERROR_HEADER`]'s `<code>;<fault>`; `None` for an answer that
    /// carried none, which is every answer of a service that is not query-compatible
    /// and every class such a service's model states no legacy code for.
    pub(super) fn query_error_code(&self) -> Option<String> {
        let carried = self.header(QUERY_ERROR_HEADER)?;
        let code = carried.split(';').next().unwrap_or_default().trim();
        (!code.is_empty()).then(|| code.to_string())
    }
}

/// The credentials a callback signs with: the emulator's own, through the provider
/// the framework hands every handler — which falls back to its placeholder pair when
/// the router vends none (request authentication off, where nothing verifies a
/// signature). They sign this call and are never reported or logged.
pub(super) async fn credentials(
    callback: &smithy_server_runtime::EmbeddedEmulator,
) -> Result<(String, String), String> {
    use ::aws_credential_types::provider::ProvideCredentials;

    let credentials = callback
        .aws_credentials_provider()
        .provide_credentials()
        .await
        .map_err(|error| error.to_string())?;
    Ok((
        credentials.access_key_id().to_string(),
        credentials.secret_access_key().to_string(),
    ))
}

/// WHAT a request is signed with: the credential, and the session token a TEMPORARY
/// one presents beside it.
///
/// A session credential is served only with its own session token, exactly as AWS
/// serves temporary credentials, so the token travels on the request and is covered by
/// the signature. It carries no `Debug` form at all: every field is a secret or names
/// one.
pub(super) struct Signer {
    pub(super) access_key_id: String,
    pub(super) secret_access_key: String,
    pub(super) session_token: Option<String>,
}

/// SigV4-sign one in-process request to another emulated service, in place.
///
/// The settings are the ones the emulator's own verifier re-signs with
/// (`vendor/server/src/request_authentication.rs`), so the signature this produces
/// is the one that verifier computes over the same request — which is what lets a
/// task's call be served with request authentication ON.
///
/// TO LIFT: this, [`dispatch`] and [`credentials`] are the SECOND copy of the
/// signed in-process callback glue in this tree; the first is
/// `services/service-com-amazonaws-cloudformation-cloudformation/src/state/provisioning.rs`
/// (`sign_callback_request`, `dispatch`, `callback_credentials`), whose settings
/// must stay identical to the verifier's. Their shared home is the vended framework
/// beside that verifier, which cannot be edited from here, and `services/common`
/// would put the signer on every service in the workspace. Whoever lifts it takes
/// BOTH copies and CloudFormation's `tests/all/callback_signature.rs`, which pins
/// the settings, together — and a third caller must lift rather than copy again.
pub(super) fn sign(
    request: &mut ::http::Request<Vec<u8>>,
    signing_name: &str,
    region: &str,
    signed_as: &Signer,
) -> Result<(), String> {
    use ::aws_sigv4::http_request::{
        sign as sign_request, PayloadChecksumKind, SignableBody, SignableRequest, SigningSettings,
    };

    let mut credential = ::aws_credential_types::Credentials::builder()
        .provider_name("aws-emulator-sfn")
        .access_key_id(&signed_as.access_key_id)
        .secret_access_key(&signed_as.secret_access_key);
    if let Some(token) = &signed_as.session_token {
        credential = credential.session_token(token);
    }
    let identity: ::aws_smithy_runtime_api::client::identity::Identity =
        credential.build().into();
    let mut settings = SigningSettings::default();
    settings.payload_checksum_kind = PayloadChecksumKind::NoHeader;
    settings.excluded_headers = None;
    let params = ::aws_sigv4::sign::v4::SigningParams::builder()
        .identity(&identity)
        .region(region)
        .name(signing_name)
        .settings(settings)
        .time(SystemTime::now())
        .build()
        .map_err(|error| error.to_string())?;
    let body = request.body().clone();
    let signable_headers: Vec<(String, String)> = request
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let signable = SignableRequest::new(
        request.method().as_str(),
        request.uri().to_string(),
        signable_headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
        SignableBody::Bytes(&body),
    )
    .map_err(|error| error.to_string())?;
    let (instructions, _signature) = sign_request(signable, &params.into())
        .map_err(|error| error.to_string())?
        .into_parts();
    instructions.apply_to_request_http1x(request);
    Ok(())
}
