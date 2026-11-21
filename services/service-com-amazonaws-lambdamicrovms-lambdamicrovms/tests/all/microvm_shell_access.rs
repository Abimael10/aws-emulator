// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `CreateMicrovmShellAuthToken`: which MicroVMs a shell
//! authentication token is minted for, and what the token it mints GRANTS at the MicroVM's
//! published endpoint.
//!
//! Three rules, each one level more general than the case that exposed it:
//!
//! * a shell token is minted only for a MicroVM that was RUN with the AWS-managed
//!   `SHELL_INGRESS` network connector attached — whichever spelling the launch named it
//!   by, wherever in the list it sits — and every other MicroVM is refused with the
//!   modelled `ValidationException` (HTTP 400);
//! * the refusals of the operation's own address are the MicroVM's: an identifier this
//!   region holds no MicroVM for is 404, and a MicroVM that accepts no further connection
//!   is 409;
//! * the token it minted is a token of the MicroVM's published endpoint that grants that
//!   MicroVM's SHELL: the program a request carries is performed in the MicroVM's own
//!   machine, so what it wrote comes back, what it wrote to the disk is there for the next
//!   request, and a token minted for another MicroVM grants nothing.
//!
//! Each test states its rule and exercises it over a SET of inputs; the values any one row
//! happens to carry are examples, not the specification. The wire harness (`emulator`,
//! `call`, `image_body`, …) is the one `microvm_lifecycle` owns; the endpoint itself is
//! driven through `state::microvm_endpoint::serve_request`, the one decision the core's
//! HTTPS listener hands every request on that address to.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::microvm_endpoint::{
    serve_request, AUTH_TOKEN_HEADER, NO_GUEST_RESPONSE_STATUS,
};
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::microvm_network::connector_state;
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::microvm_shell::SHELL_STATUS_HEADER;
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::{
    managed_network_connector_arn, no_shell_ingress_message,
    ComAmazonawsLambdamicrovmsLambdamicrovmsState as MicrovmState, DEFAULT_INGRESS_NETWORK_CONNECTOR,
    SHELL_INGRESS_NETWORK_CONNECTOR,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::microvm_lifecycle::{
    call, emulator_with_engine, image_body, poll_until_microvm_state, run_microvm, PAST_THE_SPAN,
    REGION,
};

/// Create the image `name` with `extra` and wait out its asynchronous build, so a MicroVM
/// can be launched from it.
async fn built_image_with(router: &MultiServiceRouter, name: &str, extra: Value) {
    let (status, created) = call(
        router,
        "POST",
        "/2025-09-09/microvm-images",
        Some(image_body(name, extra)),
    )
    .await;
    assert_eq!(status, 201, "create the image {name}: got {status} {created}");
    tokio::time::sleep(PAST_THE_SPAN).await;
}

/// Run a MicroVM of `image` with `ingress` as its `ingressNetworkConnectors` (omitted
/// where the row names none) and hand back `(microvmId, endpoint)` once it is RUNNING.
async fn running_microvm(
    router: &MultiServiceRouter,
    case: &str,
    image: &str,
    ingress: Option<Value>,
) -> (String, String) {
    let mut body = json!({"imageIdentifier": image});
    if let Some(ingress) = ingress {
        body["ingressNetworkConnectors"] = ingress;
    }
    let (status, run) = run_microvm(router, body).await;
    assert_eq!(status, 200, "case {case:?}: run a MicroVM: got {status} {run}");
    let microvm_id = run["microvmId"].as_str().unwrap_or_default().to_string();
    let endpoint = run["endpoint"].as_str().unwrap_or_default().to_string();
    poll_until_microvm_state(router, case, &microvm_id, "RUNNING").await;
    (microvm_id, endpoint)
}

/// `POST /2025-09-09/microvms/{microvmIdentifier}/shell-auth-token`.
async fn create_shell_auth_token(
    router: &MultiServiceRouter,
    microvm_identifier: &str,
    expiration_in_minutes: u32,
) -> (u16, Value) {
    call(
        router,
        "POST",
        &format!("/2025-09-09/microvms/{microvm_identifier}/shell-auth-token"),
        Some(json!({"expirationInMinutes": expiration_in_minutes})),
    )
    .await
}

/// The token a successful `CreateMicrovmShellAuthToken` handed back, asserted to be a
/// `TokenParts` map carrying the key a client presents it under.
fn minted_token(case: &str, response: &Value) -> String {
    let token = response["authToken"][AUTH_TOKEN_HEADER]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !token.is_empty(),
        "case {case:?}: expected a token at {AUTH_TOKEN_HEADER:?}, got {response}"
    );
    token
}

/// Send one shell request to the MicroVM's published endpoint: `program` as the body, the
/// token in `X-aws-proxy-auth`. Answers `(status, shell status, body)`, where the shell
/// status is `None` for an answer that carries none.
///
/// `microvm_id` is the MicroVM whose own address the request is sent to: the published
/// endpoint is that address's authority, so the request carries the channel's own path.
async fn through_shell(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    microvm_id: &str,
    token: Option<&str>,
    program: &str,
) -> (u16, Option<i32>, String) {
    let mut builder = http::Request::builder().method("POST").uri("/");
    if let Some(token) = token {
        builder = builder.header(AUTH_TOKEN_HEADER, token);
    }
    let request = builder
        .body(program.as_bytes().to_vec())
        .expect("a well-formed request to a MicroVM endpoint");
    let state = engine.state::<MicrovmState>();
    let connectors = connector_state(&**engine);
    let callback = smithy_server_runtime::EmbeddedEmulator::from_router(router.clone());
    let response =
        serve_request(&state, connectors.as_ref(), &callback, microvm_id, &request).await;
    let status = response.status().as_u16();
    let shell_status = response
        .headers()
        .get(SHELL_STATUS_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i32>().ok());
    let body = String::from_utf8_lossy(response.body()).to_string();
    (status, shell_status, body)
}

/// RULE: `CreateMicrovmShellAuthToken` mints a token only for a MicroVM run with the
/// AWS-managed `SHELL_INGRESS` network connector attached — by its own name or by the ARN
/// this service mints for a managed connector, alone or beside others — and refuses every
/// other MicroVM with the modelled 400 `ValidationException` naming the requirement.
#[tokio::test(flavor = "multi_thread")]
async fn a_shell_token_is_minted_only_for_a_microvm_the_shell_connector_is_attached_to() {
    let (router, _engine) = emulator_with_engine();
    built_image_with(&router, "shell-bearing", json!({})).await;

    let shell_arn = managed_network_connector_arn(REGION, SHELL_INGRESS_NETWORK_CONNECTOR);
    let http_arn = managed_network_connector_arn(REGION, DEFAULT_INGRESS_NETWORK_CONNECTOR);
    // A connector of the ACCOUNT's own namespace, spelled with the managed connector's
    // name: an account's connector is not AWS's managed one, whatever it is called.
    let own_arn =
        format!("arn:aws:lambda:{REGION}:000000000000:network-connector:aws-network-connector:{SHELL_INGRESS_NETWORK_CONNECTOR}");

    // The rows differ along what the launch attached: whether the shell connector is
    // there at all, which spelling names it, how many connectors the list carries and
    // where in it the shell connector sits.
    let rows: Vec<(&str, Option<Value>, bool)> = vec![
        ("no ingressNetworkConnectors at all", None, false),
        (
            "the managed ingress connector that is not the shell's",
            Some(json!([DEFAULT_INGRESS_NETWORK_CONNECTOR])),
            false,
        ),
        (
            "that connector by its ARN",
            Some(json!([http_arn.clone()])),
            false,
        ),
        (
            "a connector of the account's own",
            Some(json!(["my-own-connector"])),
            false,
        ),
        (
            "the shell connector's name on a connector of the account's own",
            Some(json!([own_arn.clone()])),
            false,
        ),
        (
            "the shell connector by name",
            Some(json!([SHELL_INGRESS_NETWORK_CONNECTOR])),
            true,
        ),
        (
            "the shell connector by its ARN",
            Some(json!([shell_arn.clone()])),
            true,
        ),
        (
            "the shell connector last in a longer list",
            Some(json!([
                DEFAULT_INGRESS_NETWORK_CONNECTOR,
                "my-own-connector",
                SHELL_INGRESS_NETWORK_CONNECTOR
            ])),
            true,
        ),
        (
            "the shell connector first in a longer list",
            Some(json!([shell_arn.clone(), http_arn.clone()])),
            true,
        ),
    ];

    let mut minted: Vec<String> = Vec::new();
    for (case, ingress, expected) in rows {
        let (microvm_id, _) = running_microvm(&router, case, "shell-bearing", ingress).await;
        // `expirationInMinutes` is `@range(min: 1)`: the smallest value the member allows
        // and a longer one are both minted, because the expiry scopes the token rather
        // than deciding whether there is one.
        for expiration_in_minutes in [1, 60] {
            let (status, response) =
                create_shell_auth_token(&router, &microvm_id, expiration_in_minutes).await;
            if expected {
                assert_eq!(
                    status, 200,
                    "case {case:?} for {expiration_in_minutes} minutes: expected a minted \
                     token, got {status} {response}"
                );
                minted.push(minted_token(case, &response));
            } else {
                assert_eq!(
                    status, 400,
                    "case {case:?} for {expiration_in_minutes} minutes: expected the \
                     documented requirement to refuse this MicroVM, got {status} {response}"
                );
                assert_eq!(
                    response["__type"].as_str(),
                    Some("ValidationException"),
                    "case {case:?}: expected ValidationException, got {response}"
                );
                assert_eq!(
                    response["message"].as_str(),
                    Some(no_shell_ingress_message().as_str()),
                    "case {case:?}: expected the requirement's own sentence, got {response}"
                );
            }
        }
    }

    // Every mint is its own secret: nothing is handed out twice, so two callers never
    // share one token.
    let count = minted.len();
    assert!(count >= 2, "expected several minted tokens, got {count}");
    minted.sort_unstable();
    minted.dedup();
    assert_eq!(
        minted.len(),
        count,
        "expected a distinct token per call, got {} of {count}",
        minted.len()
    );
}

/// RULE: the operation's refusals at its own address are the MicroVM's — an identifier
/// this region holds no MicroVM for is 404, whatever form it takes, and a MicroVM that
/// accepts no further connection is 409.
#[tokio::test(flavor = "multi_thread")]
async fn a_shell_token_is_refused_for_a_microvm_that_is_absent_or_on_its_way_out() {
    let (router, _engine) = emulator_with_engine();
    built_image_with(&router, "shell-refusals", json!({})).await;

    let (doomed_id, _) = running_microvm(
        &router,
        "the MicroVM about to be terminated",
        "shell-refusals",
        Some(json!([SHELL_INGRESS_NETWORK_CONNECTOR])),
    )
    .await;

    // (case, the identifier the request addresses, the status)
    let absent: Vec<(&str, String)> = vec![
        ("an id this store never minted", "mvm-ffffffffffffffff".to_string()),
        (
            "an ARN this store never minted",
            format!("arn:aws:lambda:{REGION}:000000000000:microvm:mvm-ffffffffffffffff"),
        ),
        ("a value that is no MicroVM identifier", "not-a-microvm".to_string()),
    ];
    for (case, identifier) in absent {
        let (status, response) = create_shell_auth_token(&router, &identifier, 15).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected a not-found refusal, got {status} {response}"
        );
        assert_eq!(
            response["__type"].as_str(),
            Some("ResourceNotFoundException"),
            "case {case:?}: expected ResourceNotFoundException, got {response}"
        );
    }

    // A MicroVM on its way out accepts no connection, so there is no shell to reach: the
    // modelled conflict with the resource's current state, from the instant the
    // termination starts and still once it has settled.
    let (status, terminated) = call(
        &router,
        "DELETE",
        &format!("/2025-09-09/microvms/{doomed_id}"),
        None,
    )
    .await;
    assert_eq!(status, 200, "terminate the MicroVM: got {status} {terminated}");
    for case in ["while it is terminating", "once it has terminated"] {
        let (status, response) = create_shell_auth_token(&router, &doomed_id, 15).await;
        assert_eq!(
            status, 409,
            "case {case:?}: expected a conflict with the MicroVM's state, got {status} {response}"
        );
        assert_eq!(
            response["__type"].as_str(),
            Some("ConflictException"),
            "case {case:?}: expected ConflictException, got {response}"
        );
        poll_until_microvm_state(&router, case, &doomed_id, "TERMINATED").await;
    }
}

/// RULE: the token the operation minted grants that MicroVM's SHELL at its published
/// endpoint — the program a request carries is PERFORMED in the MicroVM's own machine, so
/// the lines it wrote and the status it ended on come back, what it wrote to the disk is
/// there for the next request, and the environment it sees is the one its image declared —
/// while a token minted for another MicroVM grants nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_shell_token_grants_the_microvms_own_shell() {
    let (router, engine) = emulator_with_engine();
    built_image_with(
        &router,
        "shell-running",
        json!({"environmentVariables": {"STAGE": "prod"}}),
    )
    .await;

    let (microvm_id, _endpoint) = running_microvm(
        &router,
        "the MicroVM whose shell is reached",
        "shell-running",
        Some(json!([SHELL_INGRESS_NETWORK_CONNECTOR])),
    )
    .await;
    let (other_id, _other_endpoint) = running_microvm(
        &router,
        "another MicroVM of the same image",
        "shell-running",
        Some(json!([SHELL_INGRESS_NETWORK_CONNECTOR])),
    )
    .await;

    let (status, response) = create_shell_auth_token(&router, &microvm_id, 15).await;
    assert_eq!(status, 200, "mint a shell token: got {status} {response}");
    let token = minted_token("the MicroVM whose shell is reached", &response);

    // (case, the program, the status the shell ends on, the lines it wrote)
    let rows: Vec<(&str, &str, i32, Vec<&str>)> = vec![
        ("a command that writes a line", "echo one", 0, vec!["one"]),
        (
            "several commands of one program",
            "echo one\necho two",
            0,
            vec!["one", "two"],
        ),
        (
            "the environment the image declared",
            "echo $STAGE",
            0,
            vec!["prod"],
        ),
        (
            "a command that reported failure",
            "false",
            1,
            vec![],
        ),
        (
            "a command this MicroVM has no program for",
            "systemctl restart nginx",
            127,
            // The shell's own diagnostic for a command its search resolved nothing for,
            // which a terminal shows beside what a program wrote to standard output.
            vec!["sh: systemctl: command not found"],
        ),
        (
            "a file written to the MicroVM's disk",
            "echo kept > /data/note.txt",
            0,
            vec![],
        ),
        (
            "that file read back by a LATER request",
            "cat /data/note.txt",
            0,
            vec!["kept"],
        ),
        (
            "a path the MicroVM's disk holds nothing at",
            "cat /data/absent.txt",
            1,
            // The utility's own diagnostic, which a terminal shows beside what a program
            // wrote to standard output.
            vec!["cat: /data/absent.txt: No such file or directory"],
        ),
    ];
    for (case, program, expected_status, expected_lines) in rows {
        let (status, shell_status, body) =
            through_shell(&router, &engine, &microvm_id, Some(&token), program).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the shell was reached, so the channel answers, got {status} {body:?}"
        );
        assert_eq!(
            shell_status,
            Some(expected_status),
            "case {case:?}: expected the shell to end on {expected_status}, got {shell_status:?} \
             {body:?}"
        );
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(
            lines, expected_lines,
            "case {case:?}: expected the program's own lines, got {lines:?}"
        );
    }

    // A token is a token of ONE MicroVM: the same live token presented at another
    // MicroVM's address grants nothing there, and neither does no token at all.
    for (case, address, presented) in [
        (
            "the token at another MicroVM's address",
            &other_id,
            Some(token.as_str()),
        ),
        ("no token at all", &microvm_id, None),
        (
            "a token value nothing minted",
            &microvm_id,
            Some("not-a-token-this-emulator-minted"),
        ),
    ] {
        let (status, shell_status, body) =
            through_shell(&router, &engine, address, presented, "echo one").await;
        assert_eq!(
            status, 403,
            "case {case:?}: expected the endpoint to refuse, got {status} {body:?}"
        );
        assert_eq!(
            shell_status, None,
            "case {case:?}: a refused request performed no program, got {shell_status:?}"
        );
    }

    // A token of `CreateMicrovmAuthToken` grants PORTS of the endpoint, never the shell:
    // its request is handed to the server the MicroVM's guest bound, which for an image
    // whose code artifact this emulator holds nothing for is the proxy's own answer for a
    // MicroVM it reached nothing inside of — and it carries no shell status.
    let (status, ports) = call(
        &router,
        "POST",
        &format!("/2025-09-09/microvms/{microvm_id}/auth-token"),
        Some(json!({"expirationInMinutes": 15, "allowedPorts": [{"allPorts": {}}]})),
    )
    .await;
    assert_eq!(status, 200, "mint a port token: got {status} {ports}");
    let port_token = minted_token("a token scoped to the endpoint's ports", &ports);
    let (status, shell_status, body) =
        through_shell(&router, &engine, &microvm_id, Some(&port_token), "echo one").await;
    assert_eq!(
        status, NO_GUEST_RESPONSE_STATUS,
        "a port token reaches the guest rather than the shell, got {status} {body:?}"
    );
    assert_eq!(
        shell_status, None,
        "a port token performs no shell program, got {shell_status:?}"
    );

    // What the shell wrote to the disk is the MicroVM's: the file the table left behind is
    // still there for a token minted after it.
    let (status, response) = create_shell_auth_token(&router, &microvm_id, 15).await;
    assert_eq!(status, 200, "mint a second shell token: got {status} {response}");
    let next = minted_token("a token minted after the writes", &response);
    let (status, shell_status, body) =
        through_shell(&router, &engine, &microvm_id, Some(&next), "cat /data/note.txt").await;
    assert_eq!(status, 200, "the shell answers the second token, got {status} {body:?}");
    assert_eq!(
        shell_status,
        Some(0),
        "the file is still on the MicroVM's disk, got {shell_status:?} {body:?}"
    );
    assert_eq!(
        body.lines().collect::<Vec<&str>>(),
        vec!["kept"],
        "expected what an earlier request wrote, got {body:?}"
    );
}
