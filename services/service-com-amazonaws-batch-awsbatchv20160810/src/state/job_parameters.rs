// SPDX-License-Identifier: CC0-1.0

//! The PARAMETER SUBSTITUTION a job's container command carries, applied where that
//! command is about to be run.
//!
//! `RegisterJobDefinition$parameters` is "default parameter substitution PLACEHOLDERS to set
//! in the job definition", and `SubmitJob$parameters` is "additional parameters passed to the
//! job that REPLACE parameter substitution placeholders that are set in the job definition …
//! Parameters in a `SubmitJob` request override any corresponding parameter defaults from the
//! job definition". So a job definition's container properties may carry a placeholder for a
//! value its submit decides — in the words of its `command` and in the values of its
//! `environment` variables, which is what the container is handed — the placeholder is
//! written [`PLACEHOLDER_PREFIX`] followed by the parameter's name, and the value that
//! replaces it is the job's EFFECTIVE parameter — the submit's over the definition's default,
//! which is the map [`super::merged_parameters`] recorded on the job when it was accepted.
//!
//! The substitution belongs to the RUN, not to the record: a job's `parameters`, its
//! `container.command` and its `container.environment` are all reported as the requests
//! stated them, so a client reads the placeholders and the values that fill them, and what
//! the container is given is those properties with the values in them. One reading of that
//! container therefore serves both places a job's program is run — the Amazon ECS task it
//! runs as ([`super::ecs_task`]) and the program this process performs for a job placed on
//! capacity that can take no task ([`super::container_run`]).
//!
//! A placeholder naming a parameter the job carries no value for is left exactly as written:
//! nothing is invented for it, so the container is given the word the job definition declared
//! and the run reports what that word did.

use super::*;

/// What a parameter substitution placeholder begins with: the name of the parameter follows
/// it immediately.
///
/// The form AWS Batch documents for the placeholders `parameters` fills (`Ref::codec` for the
/// parameter `codec`).
pub const PLACEHOLDER_PREFIX: &str = "Ref::";

/// The characters a parameter NAME may be made of, which is what decides where a placeholder
/// ends inside a longer word.
///
/// `ParametersMap`'s keys are plain `String`s in the model, so the boundary is this: a
/// placeholder runs to the first character a parameter name cannot contain, which is what
/// lets `Ref::code;` and `Ref::code/shard` carry a placeholder followed by punctuation the
/// command keeps.
fn is_parameter_name_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_' || character == '-' || character == '.'
}

/// How many bytes ONE job's whole substitution may ADD to what its container declared.
///
/// THIS NUMBER IS THE EMULATOR'S, and it is the bound on the growth of the WHOLE
/// substitution rather than of one word: both the words and the values come from requests
/// anyone can repeat, so without one budget for all of them the output would be bounded by
/// the PRODUCT of the word count and the longest value, and the request chooses both factors.
/// It is the product of this emulator's existing ceilings on a container's argument vector
/// ([`MAX_PROGRAM_WORDS`] words of [`MAX_PROGRAM_WORD_BYTES`]), so a substitution can never
/// add more than one performable vector's worth to what was stored.
pub const MAX_SUBSTITUTED_GROWTH_BYTES: usize = MAX_PROGRAM_WORDS * MAX_PROGRAM_WORD_BYTES;

/// What one job's substitution has left to spend ([`MAX_SUBSTITUTED_GROWTH_BYTES`]).
///
/// Threaded through every word and every variable value of ONE container, so the budget is
/// the whole substitution's and not each word's.
#[derive(Debug)]
pub struct SubstitutionBudget {
    /// The bytes the substitution may still ADD. A value longer than this is not substituted.
    remaining: usize,
}

impl Default for SubstitutionBudget {
    fn default() -> Self {
        Self {
            remaining: MAX_SUBSTITUTED_GROWTH_BYTES,
        }
    }
}

impl SubstitutionBudget {
    /// Draw the growth of replacing a `placeholder`-byte placeholder with a `value`-byte value,
    /// and answer whether the budget covered it. A substitution that shrinks the word costs
    /// nothing.
    fn covers(&mut self, placeholder: usize, value: usize) -> bool {
        let growth = value.saturating_sub(placeholder);
        if growth > self.remaining {
            return false;
        }
        self.remaining -= growth;
        true
    }
}

/// `word` with every parameter substitution placeholder in it replaced by the value
/// `parameters` gives that parameter, drawing the growth from `budget`.
///
/// One word may carry several placeholders and text around them, so the word is walked once
/// and each placeholder replaced where it stands. A placeholder whose parameter is not in
/// `parameters`, and one whose value the budget cannot cover, is copied through unchanged —
/// nothing is invented and nothing grows past the bound.
#[must_use]
pub fn substitute_parameters(
    word: &str,
    parameters: &HashMap<String, String>,
    budget: &mut SubstitutionBudget,
) -> String {
    let mut filled = String::with_capacity(word.len());
    let mut rest = word;
    while let Some(at) = rest.find(PLACEHOLDER_PREFIX) {
        let (before, placeholder) = rest.split_at(at);
        filled.push_str(before);
        let named = &placeholder[PLACEHOLDER_PREFIX.len()..];
        let end = named
            .find(|character| !is_parameter_name_char(character))
            .unwrap_or(named.len());
        let (name, after) = named.split_at(end);
        let written = parameters.get(name).filter(|value| {
            budget.covers(PLACEHOLDER_PREFIX.len().saturating_add(name.len()), value.len())
        });
        match written {
            Some(value) => filled.push_str(value),
            None => {
                filled.push_str(PLACEHOLDER_PREFIX);
                filled.push_str(name);
            }
        }
        rest = after;
    }
    filled.push_str(rest);
    filled
}

/// `command` with the placeholders in each of its words filled from `parameters`, and
/// `command` itself when the job carries no parameters at all.
///
/// Only the first [`MAX_PROGRAM_WORDS`] words are substituted into: a longer vector names no
/// program this emulator performs at all ([`container_program`]), so there is nothing for a
/// substitution past that ceiling to decide.
#[must_use]
pub fn substituted_command(
    command: Option<&Vec<String>>,
    parameters: Option<&HashMap<String, String>>,
    budget: &mut SubstitutionBudget,
) -> Option<Vec<String>> {
    let command = command?;
    let Some(parameters) = parameters.filter(|parameters| !parameters.is_empty()) else {
        return Some(command.clone());
    };
    Some(
        command
            .iter()
            .enumerate()
            .map(|(position, word)| {
                if position < MAX_PROGRAM_WORDS {
                    substitute_parameters(word, parameters, budget)
                } else {
                    word.clone()
                }
            })
            .collect(),
    )
}

/// `environment` with the placeholders in each variable's VALUE filled from `parameters`,
/// drawing on the same budget the command drew on.
///
/// The variable NAMES are not substituted: a placeholder stands where a value the submit
/// decides goes, and the names are the container's own interface.
#[must_use]
pub fn substituted_environment(
    environment: Option<&Vec<KeyValuePair>>,
    parameters: Option<&HashMap<String, String>>,
    budget: &mut SubstitutionBudget,
) -> Option<Vec<KeyValuePair>> {
    let environment = environment?;
    let Some(parameters) = parameters.filter(|parameters| !parameters.is_empty()) else {
        return Some(environment.clone());
    };
    Some(
        environment
            .iter()
            .map(|pair| KeyValuePair {
                name: pair.name.clone(),
                value: pair
                    .value
                    .as_deref()
                    .map(|value| substitute_parameters(value, parameters, budget)),
            })
            .collect(),
    )
}

impl ComAmazonawsBatchAwsbatchv20160810State {
    /// The container the job `id` is actually RUN with: the effective container stored on the
    /// job when its submit was accepted ([`effective_container`] built it), with this job's
    /// parameters substituted into the words of its command and the values of its environment
    /// variables.
    ///
    /// `None` for a job this store holds no container for. The job's own record is untouched:
    /// a `DescribeJobs` reports the command, the environment and the `parameters` as the
    /// requests stated them, and this is what the container is given. The whole substitution
    /// shares ONE budget ([`SubstitutionBudget`]), so what it can add is a constant.
    pub(super) fn job_run_container(&self, id: &str) -> Option<ContainerDetail> {
        let job = self.jobs.get(id)?;
        let mut container = job.container.clone()?;
        let parameters = job.parameters.as_ref();
        let mut budget = SubstitutionBudget::default();
        container.command =
            substituted_command(container.command.as_ref(), parameters, &mut budget);
        container.environment =
            substituted_environment(container.environment.as_ref(), parameters, &mut budget);
        Some(container)
    }
}
