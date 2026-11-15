// SPDX-License-Identifier: CC0-1.0

//! The AWS CLI as a build environment provides it: the Amazon ECR login password a build's
//! commands ask for, really issued by that registry.
//!
//! # The rule this module implements
//!
//! `aws ecr get-login-password` "retrieves a token that is valid for a specified registry for
//! 12 hours" and writes the PASSWORD half of it on standard output, which is what the
//! documented login pipes into `docker login --username AWS --password-stdin` (Amazon ECR
//! User Guide, "Private registry authentication"). The token is Amazon ECR's
//! `GetAuthorizationToken`, so the command performs that call: the password a build's command
//! writes is the one the registry issued, and a role whose policies do not allow
//! `ecr:GetAuthorizationToken` gets no password at all — the command exits with the status the
//! CLI answers for a service error, exactly as the real CLI does for a denied call.
//!
//! Every other command group, every other command of this group, and every form of this one
//! carrying an option it does not take is an invocation this program performs nothing of, so
//! the session reports it as a command it could not run.

use super::{answered, service_answered, BuildTools, AWS_CLI_SERVICE_ERROR, PROGRAM_SUCCEEDED};
use ::service_common::posix_shell_session::ProgramOutcome;

/// The name a command line invokes the AWS CLI by.
pub const AWS_CLI_PROGRAM: &str = "aws";

/// The CLI's command GROUP this program answers for: `aws ecr`.
const ECR_COMMAND_GROUP: &str = "ecr";

/// The command of that group this program performs, as the CLI spells it.
const GET_LOGIN_PASSWORD_COMMAND: &str = "get-login-password";

/// The API operation that command performs.
const GET_AUTHORIZATION_TOKEN_OPERATION: &str = "GetAuthorizationToken";

/// The IAM action it is performed under ("Actions defined by Amazon Elastic Container
/// Registry", IAM Service Authorization Reference).
const GET_AUTHORIZATION_TOKEN_ACTION: &str = "ecr:GetAuthorizationToken";

/// The resource the action is authorized on: the call addresses the registry as a whole and
/// names no repository, which is the action IAM publishes with the resource type `*`.
const ANY_RESOURCE: &str = "*";

/// The option this program takes, which names the Region whose registry issues the token.
const REGION_OPTION: &str = "--region";

/// The user half of the `user:password` pair an Amazon ECR authorization token decodes to,
/// which is the literal the documented login presents (`docker login --username AWS`).
const TOKEN_USER: &str = "AWS";

/// What one `aws` command line asks for — `None` for every invocation this program performs
/// nothing of.
///
/// An invocation naming another command group or another command of this one, carrying an
/// option other than [`REGION_OPTION`] (every option of the CLI changes what the command does,
/// so a line carrying one this does not take is not this invocation), or carrying an operand
/// at all is one this program answers nothing for.
fn requested(arguments: &[String]) -> Option<Region> {
    let mut words = arguments.iter().map(String::as_str);
    let mut region = None;
    let mut named: Vec<&str> = Vec::new();
    while let Some(word) = words.next() {
        if let Some(value) = word.strip_prefix(REGION_OPTION) {
            region = match value.strip_prefix('=') {
                Some(value) => Some(value.to_string()),
                // `--region <value>`: the value is the next word.
                None if value.is_empty() => Some(words.next()?.to_string()),
                None => return None,
            };
            continue;
        }
        if word.starts_with('-') {
            return None;
        }
        named.push(word);
    }
    if named.len() != 2
        || named[0] != ECR_COMMAND_GROUP
        || named[1] != GET_LOGIN_PASSWORD_COMMAND
    {
        return None;
    }
    Some(Region(region.filter(|region| !region.is_empty())))
}

/// The Region whose registry a command addresses, when it names one.
struct Region(Option<String>);

/// Perform the invocation `arguments` asks for, or answer `None` for one this program
/// performs nothing of.
pub(super) fn performed(tools: &BuildTools, arguments: &[String]) -> Option<ProgramOutcome> {
    let Region(named) = requested(arguments)?;
    let region = named.unwrap_or_else(|| tools.region.clone());
    if let Some(denial) = tools.withheld(GET_AUTHORIZATION_TOKEN_ACTION, ANY_RESOURCE) {
        return Some(tools.refused(
            AWS_CLI_PROGRAM,
            &format!(
                "An error occurred (AccessDeniedException) when calling the \
                 {GET_AUTHORIZATION_TOKEN_OPERATION} operation: {denial}"
            ),
            AWS_CLI_SERVICE_ERROR,
        ));
    }
    match issued_password(tools, &region) {
        Ok(password) => {
            // The password is now one this build may present to the registry: a login
            // carrying anything else is a credential the registry never issued.
            tools.state().issued_passwords.insert(password.clone());
            Some(answered(PROGRAM_SUCCEEDED, vec![password]))
        }
        Err(detail) => Some(tools.refused(AWS_CLI_PROGRAM, &detail, AWS_CLI_SERVICE_ERROR)),
    }
}

/// The password the registry of `region` issues this build, through that registry's own
/// `GetAuthorizationToken`.
///
/// `Err` carries what the call answered, as the CLI reports a failed call.
fn issued_password(tools: &BuildTools, region: &str) -> Result<String, String> {
    let token = tools.made(|| async {
        let answer = tools
            .ecr(region)
            .get_authorization_token()
            .send()
            .await
            .map_err(|error| service_answered(GET_AUTHORIZATION_TOKEN_OPERATION, &error))?;
        answer
            .authorization_data()
            .first()
            .and_then(|data| data.authorization_token.clone())
            .ok_or_else(|| {
                format!(
                    "The {GET_AUTHORIZATION_TOKEN_OPERATION} operation answered no \
                     authorization data"
                )
            })
    })?;
    // "The authorizationToken returned is a base64 encoded string that can be decoded and
    // used in a docker login command": the decoded form is `user:password`, and the password
    // is the half the CLI writes.
    let decoded = ::aws_smithy_types::base64::decode(&token)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .ok_or_else(|| "The authorization token the registry issued is not readable".to_string())?;
    let (user, password) = decoded
        .split_once(':')
        .ok_or_else(|| "The authorization token the registry issued names no password".to_string())?;
    if user != TOKEN_USER || password.is_empty() {
        return Err(
            "The authorization token the registry issued names no registry password".to_string(),
        );
    }
    Ok(password.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the table below: the case, the words of the command line, and the
    /// invocation they ask for — with the Region it names, when it names one.
    type Row = (&'static str, Vec<&'static str>, Option<Option<&'static str>>);

    /// RULE: the only invocation this program performs is `aws ecr get-login-password`, with
    /// the Region it names when it names one.
    ///
    /// The rows differ along what the rule generalises over: how the Region option is
    /// written, which command group and command are named, and which other forms carry an
    /// option or an operand this program does not take.
    #[test]
    fn only_the_ecr_login_password_command_is_performed() {
        // (case, the words, the Region the invocation names)
        let rows: Vec<Row> = vec![
            (
                "the command with no option",
                vec!["ecr", "get-login-password"],
                Some(None),
            ),
            (
                "a separated Region option",
                vec!["ecr", "get-login-password", "--region", "eu-west-1"],
                Some(Some("eu-west-1")),
            ),
            (
                "an attached Region option",
                vec!["ecr", "get-login-password", "--region=eu-west-1"],
                Some(Some("eu-west-1")),
            ),
            (
                "the option written before the command",
                vec!["--region", "us-east-2", "ecr", "get-login-password"],
                Some(Some("us-east-2")),
            ),
            (
                "another command of the same group",
                vec!["ecr", "describe-repositories"],
                None,
            ),
            ("another command group", vec!["s3", "ls"], None),
            (
                "an option this program does not take",
                vec!["ecr", "get-login-password", "--no-verify-ssl"],
                None,
            ),
            (
                "an operand past the command",
                vec!["ecr", "get-login-password", "extra"],
                None,
            ),
            ("no words at all", vec![], None),
        ];
        for (case, words, expected) in rows {
            let arguments: Vec<String> = words.iter().map(|word| (*word).to_string()).collect();
            let asked: Option<Option<String>> =
                requested(&arguments).map(|Region(region)| region);
            let want: Option<Option<String>> =
                expected.map(|region| region.map(str::to_string));
            assert_eq!(asked, want, "case {case:?}: expected {want:?}, got {asked:?}");
        }
    }
}
