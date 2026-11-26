// SPDX-License-Identifier: CC0-1.0

//! The AWS CLI's `aws ssm get-parameter`, as a program a caller-supplied command line can ask
//! for: what one command line asks of the CLI, and the call that reads the parameter it names.
//!
//! # The rule this module implements
//!
//! Several emulated services run a command line a client supplied — a container's `command`, a
//! cluster step's arguments, a build's commands — and such a line reaches a service of the
//! account through the AWS CLI, which "provides direct access to the public APIs of Amazon Web
//! Services" one command at a time. `aws ssm get-parameter` "gets information about a single
//! parameter" (`GetParameter`), so a command line that reads a parameter is answered the value
//! that parameter holds in the account — which is how a workload takes its configuration from the
//! parameter store, and how a change a client puts there reaches the next read. The status the
//! command exits with is the status of that call, and WHO made it is whichever principal the
//! session running the line signs with: a workload whose role does not carry the action is
//! refused, and the command reports that refusal verbatim as the CLI words a service's own.
//!
//! That is ONE rule, so it lives here once rather than per service, beside the sibling command
//! groups ([`crate::aws_s3_cli`], [`crate::aws_sns_cli`], [`crate::aws_cloudwatch_cli`],
//! [`crate::aws_xray_cli`]). What each service keeps is its own — the SESSION the line runs in and
//! WHICH credential the call is signed with.
//!
//! Nothing here builds a client, resolves a name, opens a socket or starts a process. The client
//! these functions are HANDED is the one this workspace constructs for that hop
//! ([`crate::ssm_client`]), whose HTTP client IS the emulator's own in-process callback
//! (`smithy_server_runtime::EmbeddedEmulator::http_client`): a `send()` on it is dispatched inside
//! this process to the emulated Systems Manager service and reaches nothing off it — the SDK's own
//! HTTPS stack is not even compiled in (`aws-sdk-ssm` is declared `default-features = false`), and
//! the endpoint URL that client carries is addressing for that transport rather than a host, built
//! from the Region alone. So the Region a command line names selects an endpoint of THIS emulator
//! and can never be an address, and the parameter a command line names is a parameter of this
//! emulator.

use crate::aws_cli_options::{CommandLine, OutputFormat, GLOBAL_PARAMETERS};

/// The CLI's command GROUP this program answers for: `aws ssm`, the commands of AWS Systems
/// Manager.
pub const SSM_COMMAND_GROUP: &str = "ssm";

/// The command of that group this program performs, as the CLI spells it: `get-parameter`.
/// SOURCE: the `aws ssm` command reference. Every other command of the group — and every other
/// group — is one this program performs nothing of.
pub const PERFORMED_SSM_COMMANDS: [&str; 1] = [GET_PARAMETER_COMMAND];

/// `get-parameter`, the one command of [`PERFORMED_SSM_COMMANDS`].
const GET_PARAMETER_COMMAND: &str = "get-parameter";

/// The API operation that command calls, as the CLI names it in its account of a refusal.
pub const GET_PARAMETER_OPERATION: &str = "GetParameter";

/// "`--name` (string) The name or Amazon Resource Name (ARN) of the parameter that you want to
/// query" (`GetParameter$Name`, required).
const NAME_PARAMETER: &str = "name";

/// "`--with-decryption` | `--no-with-decryption` (boolean) Return decrypted values for secure
/// string parameters" (`GetParameter$WithDecryption`) — a switch, so it carries no value.
const WITH_DECRYPTION_PARAMETER: &str = "with-decryption";
const WITHOUT_DECRYPTION_PARAMETER: &str = "no-with-decryption";

/// What ONE `aws ssm get-parameter` command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetParameterCall {
    /// The parameter the line named.
    pub name: String,
    /// Whether the line asked for the decrypted value.
    pub with_decryption: bool,
    /// `--region`, and `None` for a line naming none.
    pub region: Option<String>,
    /// `--output`, the format the answer is printed in.
    pub output: OutputFormat,
}

/// What the command line `arguments` — the words AFTER the program name — asks of the CLI's
/// Systems Manager commands, or `None` for every invocation this program performs nothing of.
///
/// It performs [`GET_PARAMETER_COMMAND`] over the parameters that command takes and the options
/// every command takes. Every other command of the group, every other parameter, a line naming no
/// parameter, and one asking for the decrypted value and the encrypted one at once, is answered
/// nothing for.
#[must_use]
pub fn requested_get_parameter(arguments: &[String]) -> Option<GetParameterCall> {
    let mut words = arguments.iter().map(String::as_str);
    if words.next()? != SSM_COMMAND_GROUP {
        return None;
    }
    if !PERFORMED_SSM_COMMANDS.contains(&words.next()?) {
        return None;
    }
    let line = CommandLine::read(
        words,
        &[WITH_DECRYPTION_PARAMETER, WITHOUT_DECRYPTION_PARAMETER],
        &[],
    )?;
    let mut taken: Vec<&str> = vec![
        NAME_PARAMETER,
        WITH_DECRYPTION_PARAMETER,
        WITHOUT_DECRYPTION_PARAMETER,
    ];
    taken.extend(GLOBAL_PARAMETERS);
    if !line.names_only(&taken) {
        return None;
    }
    if line.carries(WITH_DECRYPTION_PARAMETER) && line.carries(WITHOUT_DECRYPTION_PARAMETER) {
        return None;
    }
    Some(GetParameterCall {
        // The name is the one member the call requires, so a line naming none is not an
        // invocation.
        name: line.one(NAME_PARAMETER)?.to_string(),
        with_decryption: line.carries(WITH_DECRYPTION_PARAMETER),
        region: line.region(),
        output: match line.one(crate::aws_cli_options::OUTPUT_PARAMETER) {
            Some(written) => OutputFormat::of(written)?,
            None => OutputFormat::default(),
        },
    })
}

/// Read the parameter `call` names through `client`, and answer what the CLI prints for the call:
/// the response document, in the format the line asked for.
///
/// `client` is the in-process hop described in [the module docs](self): its transport is the
/// emulator's own callback, so the `send()` below dispatches to the emulated Systems Manager
/// service inside this process and reaches nothing off it.
///
/// The Region the line named ([`GetParameterCall::region`]) is the CALLER's to apply: it addresses
/// the client it hands over — which is to say which emulated endpoint of this process that client
/// is built for — exactly as the sibling groups' calls do.
///
/// `Err` carries what the CLI reports about a call the service refused — its own error code and
/// message, in the CLI's words for one ([`crate::aws_cli_options::service_answered`]).
pub async fn read_parameter(
    client: &::aws_sdk_ssm::Client,
    call: &GetParameterCall,
) -> Result<String, String> {
    use ::aws_sdk_ssm::primitives::DateTimeFormat;

    let answered = client
        .get_parameter()
        .name(&call.name)
        .with_decryption(call.with_decryption)
        .send()
        .await
        .map_err(|error| {
            crate::aws_cli_options::service_answered(GET_PARAMETER_OPERATION, &error)
        })?;
    // The one member the operation answers (`GetParameterResult$Parameter`), carrying the members
    // the service answered and no others — a member it answered nothing for is absent, never
    // filled in.
    let mut parameter = ::serde_json::Map::new();
    let held = answered.parameter();
    for (member, value) in [
        ("Name", held.and_then(|held| held.name())),
        (
            "Type",
            held.and_then(|held| held.r#type()).map(|held| held.as_str()),
        ),
        ("Value", held.and_then(|held| held.value())),
        ("Selector", held.and_then(|held| held.selector())),
        ("SourceResult", held.and_then(|held| held.source_result())),
        ("ARN", held.and_then(|held| held.arn())),
        (
            "DataType",
            held.and_then(|held| held.data_type()),
        ),
    ] {
        if let Some(value) = value {
            parameter.insert(member.to_string(), ::serde_json::Value::String(value.to_string()));
        }
    }
    if let Some(version) = held.map(|held| held.version()).filter(|version| *version != 0) {
        parameter.insert(
            "Version".to_string(),
            ::serde_json::Value::Number(version.into()),
        );
    }
    if let Some(stamped) = held
        .and_then(|held| held.last_modified_date())
        .and_then(|at| at.fmt(DateTimeFormat::DateTime).ok())
    {
        parameter.insert(
            "LastModifiedDate".to_string(),
            ::serde_json::Value::String(stamped),
        );
    }
    let document = ::serde_json::json!({ "Parameter": ::serde_json::Value::Object(parameter) });
    Ok(crate::aws_cli_options::printed_answer(&document, call.output))
}

#[cfg(test)]
mod tests {
    use super::{requested_get_parameter, GetParameterCall};
    use crate::aws_cli_options::OutputFormat;

    /// The words of a command line after the program name, as a session hands them to a program.
    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(ToString::to_string).collect()
    }

    /// One call reading `name` with no other option named.
    fn call(name: &str) -> GetParameterCall {
        GetParameterCall {
            name: name.to_string(),
            with_decryption: false,
            region: None,
            output: OutputFormat::default(),
        }
    }

    /// RULE: `aws ssm get-parameter` over the parameters that command takes is what this program
    /// performs of the Systems Manager group, and every other invocation is one it answers nothing
    /// for — so the session reports it as a command it could not run rather than as a call.
    ///
    /// The rows differ along what the rule generalises over: how the parameter is addressed (a
    /// name, a path, an ARN), the two spellings of a value, the switch and its negation, and the
    /// options that decide how the answer is printed.
    #[test]
    fn the_invocations_this_program_performs() {
        // (case, the arguments after `aws`, what it asks for)
        let rows: [(&str, &str, Option<GetParameterCall>); 10] = [
            (
                "a parameter addressed by name",
                "ssm get-parameter --name mode",
                Some(call("mode")),
            ),
            (
                "a parameter addressed by path, written with an equals sign",
                "ssm get-parameter --name=/flint/prod/latency-mode",
                Some(call("/flint/prod/latency-mode")),
            ),
            (
                "a parameter addressed by ARN",
                "ssm get-parameter --name arn:aws:ssm:us-west-2:000000000000:parameter/flint/mode",
                Some(call(
                    "arn:aws:ssm:us-west-2:000000000000:parameter/flint/mode",
                )),
            ),
            (
                "the decrypted value asked for",
                "ssm get-parameter --name /flint/secret --with-decryption",
                Some(GetParameterCall {
                    with_decryption: true,
                    ..call("/flint/secret")
                }),
            ),
            (
                "the switch's negation, which asks for the stored value",
                "ssm get-parameter --name /flint/secret --no-with-decryption",
                Some(call("/flint/secret")),
            ),
            (
                "the answer printed as tab-separated values",
                "ssm get-parameter --name /flint/mode --output text",
                Some(GetParameterCall {
                    output: OutputFormat::Text,
                    ..call("/flint/mode")
                }),
            ),
            (
                // `--query` would FILTER the answer by evaluating an expression a caller wrote,
                // which this program performs none of, so a line carrying it is no invocation.
                "a line asking for one member of the answer to be selected",
                "ssm get-parameter --name /flint/mode --query Parameter.Value --output text",
                None,
            ),
            (
                "another command of the group",
                "ssm put-parameter --name /flint/mode --value degraded",
                None,
            ),
            ("a line naming no parameter", "ssm get-parameter", None),
            (
                "a line asking for the decrypted value and the stored one at once",
                "ssm get-parameter --name /flint/secret --with-decryption --no-with-decryption",
                None,
            ),
        ];
        for (case, line, expected) in rows {
            let asked = requested_get_parameter(&words(line));
            assert_eq!(
                asked, expected,
                "case {case:?}: expected {expected:?}, got {asked:?}"
            );
        }
    }
}
