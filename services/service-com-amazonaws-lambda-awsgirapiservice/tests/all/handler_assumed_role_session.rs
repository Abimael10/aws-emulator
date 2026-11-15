// SPDX-License-Identifier: CC0-1.0

//! WHICH IDENTITY a running handler's own AWS SDK call is signed as: Lambda, Amazon S3, IAM
//! and the token service on ONE engine, a `python3.x` function whose code does `import boto3`,
//! assumes a second role and then calls a service with the session it was vended.
//!
//! The rule pinned here: the AWS SDK a managed runtime supplies signs each call with the
//! credentials the CLIENT was built with — the ones the handler passed that client, and
//! otherwise the session of the function's own execution role — so the service serving the
//! call judges it against the policies in force on the identity those credentials name. A
//! handler that calls `sts:AssumeRole` and builds a client with the vended session therefore
//! calls as THAT session: an object the execution role may not read is read with it, and the
//! token service reports the assumed-role session as the caller. An assume the role's own
//! trust policy does not admit is the token service's own `AccessDenied`, which the handler
//! catches like any other refusal — so both the served and the refused path answer the
//! invocation with the handler's own payload and NO `FunctionError`, because neither is a
//! fault of the run.
//!
//! The rows differ along what the rule generalises over: WHICH credentials the client holds
//! (none, the session one role vended, a second session of the same role under another session
//! name, the session a role that does not trust the function would vend, part of a credential,
//! a session token alone), and therefore what the identity behind them may do to the one object
//! every row reads.

use serde_json::{json, Value};

use crate::handler_s3_object_access::{
    role_arn, session_arn, Emulator, ACCESS_DENIED, ACCESS_DENIED_STATUS,
};
use crate::invoke_request_id::one_invocation_stream_at_a_time;

/// The bucket holding the one object every row reads, and that object's key and bytes.
const BUCKET: &str = "handler-assumed-session-restricted";
const KEY: &str = "restricted/report.csv";
const BODY: &str = "id,total\n7,99\n";

/// The function every row invokes, and the execution role it runs as: that role may assume the
/// two roles below and may read NOTHING of the bucket.
const FUNCTION: &str = "handler-assumed-session";
const EXEC_ROLE: &str = "lambda-assumed-session-exec";

/// The role whose trust policy names the execution role and whose own document allows the
/// read: the session it vends is what a row's client is built with.
const READER_ROLE: &str = "lambda-assumed-session-reader";

/// The role whose trust policy names ANOTHER role of this account and not the execution role,
/// so the function's assume of it is refused.
const CLOSED_ROLE: &str = "lambda-assumed-session-closed";

/// That other role, which exists so the closed role's trust policy names a principal of this
/// account that is simply not the function's.
const OTHER_ROLE: &str = "lambda-assumed-session-other";

/// The IAM action an assume performs, which the refusal of one names.
const ASSUME_ROLE_ACTION: &str = "sts:AssumeRole";

/// The class the SDK itself refuses a client built with part of a credential with, and the
/// member such a refusal must name as missing.
const PARTIAL_CREDENTIALS: &str = "PartialCredentialsError";
const MISSING_MEMBER: &str = "aws_secret_access_key";

/// The handler every row runs: it builds its clients with the credentials the row names —
/// none, or the session it assumed — reports the identity the token service says those calls
/// are made as, and reports the object's bytes or the code the read was refused with.
const SOURCE: &str = r#"
import boto3
import botocore.exceptions


def identity(**credentials):
    """Who the token service says a client built with these credentials calls as."""
    return boto3.client("sts", **credentials).get_caller_identity()["Arn"]


def read(event, **credentials):
    """The object's bytes, or the code and sentence the read was refused with."""
    client = boto3.client("s3", **credentials)
    try:
        answered = client.get_object(Bucket=event["bucket"], Key=event["key"])
    except botocore.exceptions.ClientError as error:
        return {
            "read": False,
            "code": error.response["Error"]["Code"],
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    return {"read": True, "body": answered["Body"].read().decode("utf-8")}


def vended(event):
    """The session credentials the token service vends for the role the event names."""
    answered = boto3.client("sts").assume_role(
        RoleArn=event["role_arn"], RoleSessionName=event["session"]
    )
    held = answered["Credentials"]
    credentials = {
        "aws_access_key_id": held["AccessKeyId"],
        "aws_secret_access_key": held["SecretAccessKey"],
        "aws_session_token": held["SessionToken"],
    }
    return credentials, answered["AssumedRoleUser"]["Arn"]


def lambda_handler(event, context):
    mode = event["mode"]
    if mode == "own":
        report = read(event)
        report["identity"] = identity()
        return report
    if mode == "token_only":
        # A session token alone is not the explicit pair an SDK signs with, so these calls
        # are made as the invocation's own identity.
        report = read(event, aws_session_token=event["token"])
        report["identity"] = identity(aws_session_token=event["token"])
        return report
    if mode == "assumed":
        credentials, assumed = vended(event)
        report = read(event, **credentials)
        report["identity"] = identity(**credentials)
        report["assumed"] = assumed
        return report
    if mode == "refused":
        try:
            vended(event)
        except botocore.exceptions.ClientError as error:
            return {
                "assumed": False,
                "code": error.response["Error"]["Code"],
                "message": error.response["Error"]["Message"],
                "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
            }
        return {"assumed": True}
    try:
        boto3.client("sts", aws_access_key_id=event["key_id"])
    except botocore.exceptions.PartialCredentialsError as error:
        return {"built": False, "refused": type(error).__name__, "message": str(error)}
    return {"built": True}
"#;

/// WHICH credentials one row's clients are built with.
#[derive(Debug, Clone, Copy)]
enum Built {
    /// With no credentials: the call is signed as the identity the invocation was given.
    NoCredentials,
    /// With the session the named role vends, under the session name the row chose.
    Session(&'static str, &'static str),
    /// With part of a credential, which is no credential at all.
    PartOfOne,
    /// With a session token alone, which is not the pair an SDK signs with.
    TokenOnly,
}

/// What one row expects of the handler's report.
#[derive(Debug, Clone, Copy)]
enum Expect {
    /// The read was served, and the call was made as a session of the named role under the
    /// named session name.
    ReadsTheObject,
    /// The call was made as the execution role's own session, which may not read the object.
    RefusedTheRead,
    /// The token service refused the assume itself, naming the role the handler asked for.
    RefusedTheAssume,
    /// The SDK refused to build the client at all, so nothing was sent.
    RefusedTheClient,
}

/// One row: which credentials the handler's clients hold, and what that decides.
struct Row {
    case: &'static str,
    built: Built,
    expect: Expect,
}

const ROWS: &[Row] = &[
    // The case the rule is stated about from the function's own side: a client built with no
    // credentials is the execution role's, and that role is granted nothing of the bucket.
    Row {
        case: "a client built with no credentials calls as the execution role's own session",
        built: Built::NoCredentials,
        expect: Expect::RefusedTheRead,
    },
    // The same handler, the same object, the same function: only the credentials the client
    // holds differ, and the read is served.
    Row {
        case: "a client built with the session the reader role vended reads the object",
        built: Built::Session(READER_ROLE, "first-pass"),
        expect: Expect::ReadsTheObject,
    },
    // The session NAME is the variable part of the identity a session speaks for, so a second
    // pass under another name is another session of the same role.
    Row {
        case: "a second session of that role is named by the session name the handler chose",
        built: Built::Session(READER_ROLE, "second-pass"),
        expect: Expect::ReadsTheObject,
    },
    // A role whose trust policy names another principal of the account vends nothing to this
    // function: the token service refuses the assume, and the handler catches it.
    Row {
        case: "a role whose trust policy omits the execution role vends no session",
        built: Built::Session(CLOSED_ROLE, "third-pass"),
        expect: Expect::RefusedTheAssume,
    },
    // Part of a credential is not a credential: the SDK itself refuses, so nothing is sent and
    // no call is quietly made as the invocation's own identity instead.
    Row {
        case: "a client built with an access key id and no secret sends nothing",
        built: Built::PartOfOne,
        expect: Expect::RefusedTheClient,
    },
    // A session token alone is not the explicit pair either: such a client holds no credential
    // of its own, so its calls are the invocation's — which may not read the object.
    Row {
        case: "a client built with a session token alone calls as the execution role",
        built: Built::TokenOnly,
        expect: Expect::RefusedTheRead,
    },
];

/// The trust policy of a role that trusts ONE role of this account by ARN, which is the
/// document an assume by a function's execution role is decided by.
fn trusts(role_name: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"AWS": role_arn(role_name)},
            "Action": ASSUME_ROLE_ACTION,
        }],
    })
    .to_string()
}

/// The identity policy the two assumable roles carry: the read of the one object, which is what
/// makes a session of such a role able to do what the execution role cannot.
fn reader_document() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": "s3:GetObject",
            "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
        }],
    })
    .to_string()
}

/// The execution role's own document: the assume of the two roles this suite points it at, and
/// NOTHING of Amazon S3, so the object is out of reach of the function's own identity.
fn exec_document() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ASSUME_ROLE_ACTION,
            "Resource": [role_arn(READER_ROLE), role_arn(CLOSED_ROLE)],
        }],
    })
    .to_string()
}

/// Create one role trusted by `trusted_role`, carrying the reader document — written with
/// IAM's own operations, so the role a session is minted of is the record `iam:CreateRole`
/// wrote.
async fn assumable_role(emulator: &Emulator, case: &str, name: &str, trusted_role: &str) {
    emulator
        .query(
            case,
            "iam",
            "2010-05-08",
            "CreateRole",
            &[
                ("RoleName", name.to_string()),
                ("AssumeRolePolicyDocument", trusts(trusted_role)),
            ],
        )
        .await;
    emulator
        .query(
            case,
            "iam",
            "2010-05-08",
            "PutRolePolicy",
            &[
                ("RoleName", name.to_string()),
                ("PolicyName", "reader".to_string()),
                ("PolicyDocument", reader_document()),
            ],
        )
        .await;
}

/// The event one row hands the handler.
fn event(row: &Row) -> Value {
    let mut held = json!({"bucket": BUCKET, "key": KEY});
    let members = held.as_object_mut().expect("the event is an object");
    match row.built {
        Built::NoCredentials => {
            members.insert("mode".to_string(), json!("own"));
        }
        Built::Session(role, session) => {
            let mode = match row.expect {
                Expect::RefusedTheAssume => "refused",
                _ => "assumed",
            };
            members.insert("mode".to_string(), json!(mode));
            members.insert("role_arn".to_string(), json!(role_arn(role)));
            members.insert("session".to_string(), json!(session));
        }
        Built::PartOfOne => {
            members.insert("mode".to_string(), json!("partial"));
            // The shape of an access key id and nothing to sign with.
            members.insert("key_id".to_string(), json!("AKIAIOSFODNN7EXAMPLE"));
        }
        Built::TokenOnly => {
            members.insert("mode".to_string(), json!("token_only"));
            members.insert(
                "token".to_string(),
                json!("a-session-token-and-nothing-else"),
            );
        }
    }
    held
}

/// What one row's report must be.
fn expected(row: &Row) -> Value {
    match (row.expect, row.built) {
        (Expect::ReadsTheObject, Built::Session(role, session)) => json!({
            "read": true,
            "body": BODY,
            "identity": session_arn(role, session),
            "assumed": session_arn(role, session),
        }),
        (Expect::RefusedTheRead, _) => json!({
            "read": false,
            "code": ACCESS_DENIED,
            "status": ACCESS_DENIED_STATUS,
            "identity": session_arn(EXEC_ROLE, FUNCTION),
        }),
        (Expect::RefusedTheAssume, Built::Session(role, _session)) => json!({
            "assumed": false,
            "code": ACCESS_DENIED,
            // How AWS words an authorization refusal: the principal the call was signed as,
            // the action it asked to perform and the resource it named.
            "message": format!(
                "User: {} is not authorized to perform: {ASSUME_ROLE_ACTION} on resource: {}",
                session_arn(EXEC_ROLE, FUNCTION),
                role_arn(role),
            ),
            "status": ACCESS_DENIED_STATUS,
        }),
        (Expect::RefusedTheClient, _) => json!({
            "built": false,
            "refused": PARTIAL_CREDENTIALS,
            // botocore's own sentence for a credential it was given part of.
            "message": format!(
                "Partial credentials found in explicit, missing: {MISSING_MEMBER}"
            ),
        }),
        (expect, built) => {
            panic!("case {:?}: {expect:?} is not a row built {built:?}", row.case)
        }
    }
}

/// Every row: which identity a call a running function makes speaks for is decided by the
/// credentials the SDK client was built with, and what that identity may do decides the answer.
#[tokio::test]
async fn an_in_function_call_is_signed_as_the_credentials_its_client_holds() {
    let _stream = one_invocation_stream_at_a_time().await;
    let setup = "the function, its roles and the object of the assumed-session table";
    let emulator = Emulator::new();
    let role = emulator
        .execution_role(setup, EXEC_ROLE, Some(&exec_document()))
        .await;
    // The other role exists so the closed role's trust policy names a principal of this
    // account; it is trusted by Lambda, which is the shape every execution role carries.
    emulator.execution_role(setup, OTHER_ROLE, None).await;
    assumable_role(&emulator, setup, READER_ROLE, EXEC_ROLE).await;
    assumable_role(&emulator, setup, CLOSED_ROLE, OTHER_ROLE).await;
    emulator
        .s3_ok(setup, "PUT", &format!("/{BUCKET}"), &[])
        .await;
    emulator
        .s3_ok(setup, "PUT", &format!("/{BUCKET}/{KEY}"), BODY.as_bytes())
        .await;
    emulator.function(setup, FUNCTION, &role, SOURCE).await;

    for row in ROWS {
        let case = row.case;
        let (signal, payload) = emulator.invoke(case, FUNCTION, &event(row)).await;
        // Whatever the row's credentials decide, the RUN itself completes: the handler's own
        // payload is the answer, and no path here is a fault of the function.
        assert_eq!(
            signal, None,
            "case {case:?}: the handler must run to completion, got the function error \
             {signal:?} with {payload}"
        );
        let wanted = expected(row);
        assert_eq!(
            payload, wanted,
            "case {case:?}: expected the handler to report {wanted}, got {payload}"
        );
    }

    // The object is exactly as it was planted: every row only ever read it, and the row whose
    // identity was granted nothing read nothing at all.
    let held = emulator
        .s3_ok("the object the table read", "GET", &format!("/{BUCKET}/{KEY}"), &[])
        .await;
    assert_eq!(
        held, BODY,
        "expected the object to hold {BODY:?} after the table, got {held:?}"
    );
}
