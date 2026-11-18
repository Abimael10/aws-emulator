// SPDX-License-Identifier: CC0-1.0

//! The WORK a job template declares: its steps, each step's tasks, and the environments a
//! session running them enters.
//!
//! A job's template is the only place the work exists — `CreateJob` takes no member for a
//! step or a task — so the steps and tasks the scheduler hands to a worker are read out of
//! the document the request carried, in the language its own `templateType` names. The
//! document itself is parsed by [`super::template_document`] (one real parser per
//! language); what is here is only the walk over the tree it returns.
//!
//! ## What is read
//!
//! An Open Job Description job template (`specificationVersion: jobtemplate-2023-09`)
//! declares `steps`, each with a `name`, an optional `parameterSpace` and optional
//! `stepEnvironments`, beside the job's own `jobEnvironments`. A step's tasks are one per
//! point of its parameter space: the CARTESIAN PRODUCT of the values each
//! `taskParameterDefinitions` entry declares, which is the space when the step associates
//! its parameters no other way, and exactly one task with no parameters for a step that
//! declares no parameter space at all.
//!
//! Each value is carried to the worker as the `TaskParameterValue` member its declared type
//! names — `INT`/`FLOAT`/`STRING`/`PATH` to `int`/`float`/`string`/`path`
//! (`smithy_query(query="shape", shape="TaskParameterValue")`) — and is kept only when it
//! satisfies that member's own modeled form, so what a response carries is always a value
//! the model accepts.
//!
//! ## What is NOT read, and what that means
//!
//! [`DeclaredWork::fully_read`] is false when any part of the work is one this reader does
//! not enumerate:
//!
//! * a step whose `parameterSpace` declares a `combination` expression, which associates the
//!   declared parameters in a way other than the product;
//! * a range given as a string for any type but `INT`, an `INT` range expression that is not
//!   comma-separated `<value>` / `<start>-<end>` / `<start>-<end>:<step>` items over
//!   non-negative values, or a range carrying a `{{...}}` reference to be resolved from
//!   elsewhere in the template;
//! * a name, a value or a count the model or the bounds below cannot carry: more than
//!   [`MAX_TEMPLATE_STEPS`] steps, more than [`MAX_PARAMETERS_PER_STEP`] task parameters or
//!   [`MAX_TEMPLATE_ENVIRONMENTS`] environments on one step, a step whose product is larger
//!   than [`MAX_TASKS_PER_STEP`], a whole template whose steps together cost more than
//!   [`MAX_TEMPLATE_TASK_BYTES`] of stored work, a step or environment name the `StepName` /
//!   `EnvironmentId` shapes do not accept, or a parameter name or value the bounds below or
//!   the `TaskParameterValue` member of its declared type do not accept.
//!
//! Work that was not fully read is work this emulator cannot place on a worker, so the
//! scheduler assigns nothing from such a job (see `super::scheduling`) rather than
//! assigning a task set that is not the one the template describes. What WAS read is still
//! stored, so a reader of the job's steps reports it.

use std::collections::HashMap;

use ::serde_json::Value;

use crate::ops::types::TaskParameterValue;

/// How many steps of one job template this emulator enumerates.
///
/// A template is up to a megabyte of caller-supplied text (`JobTemplate, len 1..1000000`),
/// and every step it declares becomes stored state, so the count is bounded the way every
/// other level of this store is bounded (see [`super::MAX_FARMS`]). Far above what a render
/// job declares.
pub const MAX_TEMPLATE_STEPS: usize = 200;

/// How many tasks of one step this emulator enumerates.
///
/// The parameter space is a PRODUCT, so a short template can describe an enormous number of
/// tasks: three parameters ranging over a thousand values each is a billion of them. This is
/// the bound on what one step expands to, checked BEFORE the product is built, so no
/// template can make this process hold more than [`MAX_TEMPLATE_STEPS`] times this many
/// tasks.
pub const MAX_TASKS_PER_STEP: usize = 4096;

/// How many task parameters one step declares.
///
/// The OTHER factor of the product above: every parameter adds one entry to EVERY task of
/// the step, so the entries one step stores are the number of tasks times the number of
/// parameters, and bounding only the tasks would leave a template free to grow the second
/// factor instead. Far above the parameters a render step declares.
pub const MAX_PARAMETERS_PER_STEP: usize = 32;

/// How many environments one job or step declares.
///
/// Each becomes a stored name and a session action, so the list a template may declare is
/// bounded like everything else read out of it.
pub const MAX_TEMPLATE_ENVIRONMENTS: usize = 64;

/// How much stored work ONE template may describe, in bytes.
///
/// The bound that does not compose: the per-step bounds above multiply, so bounding each
/// factor on its own would still let a small template describe `MAX_TEMPLATE_STEPS` times
/// `MAX_TASKS_PER_STEP` times `MAX_PARAMETERS_PER_STEP` stored entries. This is the whole
/// template's allowance, spent step by step and measured the way the store measures its
/// work — [`super::TASK_BYTES`] per task plus what each of its parameters' name and value
/// cost ([`super::task_parameter_bytes`]), so a long name or value is charged for what it
/// actually holds rather than counted as one. A template that asks for more is one whose
/// work was not read, so nothing is built for the rest of it. It covers the shapes a render
/// job has — a step of a few thousand frames with a handful of parameters each, or tens of
/// smaller steps — and keeps what one request can allocate in megabytes.
///
/// What the WHOLE STORE holds is bounded separately, by the store's own budget for the work
/// of every job it holds (`super::MAX_STORED_TASK_BYTES`), so repeated creates cannot
/// compound this one either.
pub const MAX_TEMPLATE_TASK_BYTES: usize = 4 * 1024 * 1024;

/// The longest task-parameter name this reader accepts.
///
/// The name is stored with EVERY task of its step, so its length is part of what one
/// template can allocate; the model's `TaskParameters` key carries no length of its own, so
/// this is the same bound the longest value a parameter may carry has
/// ([`MAX_PARAMETER_STRING`]).
const MAX_PARAMETER_NAME: usize = 1024;

/// The `taskParameterDefinitions` type that maps to the `int` member of
/// `TaskParameterValue`, and the one whose range may be a range expression.
const PARAMETER_TYPE_INT: &str = "INT";
/// The `taskParameterDefinitions` type that maps to the `float` member.
const PARAMETER_TYPE_FLOAT: &str = "FLOAT";
/// The `taskParameterDefinitions` type that maps to the `string` member.
const PARAMETER_TYPE_STRING: &str = "STRING";
/// The `taskParameterDefinitions` type that maps to the `path` member.
const PARAMETER_TYPE_PATH: &str = "PATH";

/// The longest a step name may be: `StepName` is `len 1..512`
/// (`smithy_query(query="operation", operation="GetStep")`).
const MAX_STEP_NAME: usize = 512;

/// The longest `ParameterString` / `PathString` a `TaskParameterValue` carries: both are
/// `len 0..1024` (`smithy_query(query="shape", shape="TaskParameterValue")`).
const MAX_PARAMETER_STRING: usize = 1024;

/// The longest `FloatString` a `TaskParameterValue` carries: `len 1..26`.
const MAX_FLOAT_STRING: usize = 26;

/// The work one step of a job template declares.
#[derive(Debug, Clone, Default)]
pub struct DeclaredStep {
    /// The step's `name`, as the template declares it.
    pub name: String,
    /// The names of the step's own environments, in declared order: a session running one
    /// of this step's tasks enters them after the job's.
    pub environments: Vec<String>,
    /// One entry per task, each the parameters that task runs with. A step with no
    /// parameter space declares exactly one task, with no parameters.
    pub tasks: Vec<HashMap<String, TaskParameterValue>>,
}

/// The work a whole job template declares.
#[derive(Debug, Clone, Default)]
pub struct DeclaredWork {
    /// The names of the job's own environments, in declared order.
    pub environments: Vec<String>,
    pub steps: Vec<DeclaredStep>,
    /// Whether every part of the work above was enumerated. See this module's header for
    /// what makes it false and what the scheduler does then.
    pub fully_read: bool,
    /// What this work costs the store's own budget for it
    /// ([`MAX_TEMPLATE_TASK_BYTES`] measures one template's share of the same thing).
    pub task_bytes: usize,
}

/// The work the job template `template` declares, read with the parser `template_type`
/// names.
///
/// `Err` is a document that is not the language it said it was — the same failure
/// [`super::job_template_name`] answers for it, so a template is refused once, on the one
/// member at fault. A document that parses but declares no `steps` carries no work, which
/// is not a failure: a job can be created from a template whose `steps` is empty.
pub fn job_template_work(
    template: &str,
    template_type: &str,
) -> Result<DeclaredWork, String> {
    let Some(document) = super::template_document(template, template_type)? else {
        return Ok(DeclaredWork::default());
    };
    let mut work = DeclaredWork {
        environments: Vec::new(),
        steps: Vec::new(),
        fully_read: true,
        task_bytes: 0,
    };

    work.environments = match environment_names(document.get("jobEnvironments")) {
        Some(names) => names,
        None => {
            work.fully_read = false;
            Vec::new()
        }
    };

    let declared = match document.get("steps") {
        None | Some(Value::Null) => return Ok(work),
        Some(Value::Array(steps)) => steps,
        // `steps` is a list in every version of the specification, so a document that
        // declares something else there is one whose work this reader has not read.
        Some(_) => {
            work.fully_read = false;
            return Ok(work);
        }
    };
    if declared.len() > MAX_TEMPLATE_STEPS {
        work.fully_read = false;
    }
    let mut allowance = MAX_TEMPLATE_TASK_BYTES;
    for step in declared.iter().take(MAX_TEMPLATE_STEPS) {
        let (step, fully_read) = declared_step(step, &mut allowance);
        work.fully_read &= fully_read;
        work.steps.push(step);
    }
    work.task_bytes = MAX_TEMPLATE_TASK_BYTES.saturating_sub(allowance);
    Ok(work)
}

/// Take `bytes` out of what a template may still describe, or answer `false` for a template
/// that has already described all it may.
fn spend(allowance: &mut usize, bytes: usize) -> bool {
    match allowance.checked_sub(bytes) {
        Some(left) => {
            *allowance = left;
            true
        }
        None => false,
    }
}

/// One step of the template's `steps`, and whether all of it was read.
fn declared_step(declared: &Value, allowance: &mut usize) -> (DeclaredStep, bool) {
    let mut fully_read = true;
    let name = declared
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if name.is_empty() || name.len() > MAX_STEP_NAME {
        fully_read = false;
    }
    let environments = match environment_names(declared.get("stepEnvironments")) {
        Some(names) => names,
        None => {
            fully_read = false;
            Vec::new()
        }
    };
    let (tasks, tasks_read) = declared_tasks(declared.get("parameterSpace"), allowance);
    fully_read &= tasks_read;
    (
        DeclaredStep {
            name,
            environments,
            tasks,
        },
        fully_read,
    )
}

/// The tasks a step's `parameterSpace` declares, and whether all of them were read.
///
/// A step with no parameter space runs once, with no parameters; otherwise the tasks are the
/// product of the declared parameters' values, the last parameter varying fastest, so the
/// order a task is reported in is the order the space enumerates it.
///
/// BOTH factors of that product are bounded before any task is built: how many parameters
/// one step may declare ([`MAX_PARAMETERS_PER_STEP`]) and how many points they may describe
/// ([`MAX_TASKS_PER_STEP`]). What this step's tasks then COST is spent out of the whole
/// template's `allowance` ([`MAX_TEMPLATE_TASK_BYTES`]), charged at the widest value each
/// parameter declares, so neither how those per-step bounds compose nor how long one name or
/// value is can grow what a template builds. The tasks are built once, each from the values
/// its own index picks, rather than by re-copying every task per parameter.
fn declared_tasks(
    space: Option<&Value>,
    allowance: &mut usize,
) -> (Vec<HashMap<String, TaskParameterValue>>, bool) {
    // A task with no parameter at all is still one stored task, so it costs what a task
    // costs.
    let one_bare_task = |allowance: &mut usize| {
        if spend(allowance, super::TASK_BYTES) {
            (vec![HashMap::new()], true)
        } else {
            (Vec::new(), false)
        }
    };
    let space = match space {
        None | Some(Value::Null) => return one_bare_task(allowance),
        Some(space) => space,
    };
    // A combination expression associates the declared parameters in a way other than the
    // product, so the product is NOT this step's task set and nothing here may stand in for
    // it.
    if space.get("combination").is_some() {
        return (Vec::new(), false);
    }
    let definitions = match space.get("taskParameterDefinitions") {
        None | Some(Value::Null) => return one_bare_task(allowance),
        Some(Value::Array(definitions)) => definitions,
        Some(_) => return (Vec::new(), false),
    };
    if definitions.is_empty() {
        return one_bare_task(allowance);
    }
    if definitions.len() > MAX_PARAMETERS_PER_STEP {
        return (Vec::new(), false);
    }

    // Every parameter's values first, with the number of points they describe checked as it
    // grows, so nothing is allocated for a space that is too large to enumerate.
    let mut parameters: Vec<(String, Vec<TaskParameterValue>)> = Vec::new();
    let mut points: usize = 1;
    for definition in definitions {
        let Some((name, values)) = parameter_values(definition) else {
            return (Vec::new(), false);
        };
        if values.is_empty() {
            return (Vec::new(), false);
        }
        points = points.saturating_mul(values.len());
        if points > MAX_TASKS_PER_STEP {
            return (Vec::new(), false);
        }
        parameters.push((name, values));
    }
    // What one task of this step costs, charged at the widest value each parameter
    // declares, so the charge is an upper bound on what any point of the space holds.
    let per_task = parameters
        .iter()
        .map(|(name, values)| {
            values
                .iter()
                .map(|value| super::task_parameter_bytes(name, value))
                .max()
                .unwrap_or(0)
        })
        .fold(super::TASK_BYTES, usize::saturating_add);
    if !spend(allowance, points.saturating_mul(per_task)) {
        return (Vec::new(), false);
    }

    let mut tasks: Vec<HashMap<String, TaskParameterValue>> = Vec::with_capacity(points);
    for index in 0..points {
        let mut point = HashMap::with_capacity(parameters.len());
        let mut rest = index;
        // The last parameter varies fastest, which is the order the space enumerates its
        // points in.
        for (name, values) in parameters.iter().rev() {
            let Some(value) = values.get(rest % values.len()) else {
                return (Vec::new(), false);
            };
            rest /= values.len();
            point.insert(name.clone(), value.clone());
        }
        tasks.push(point);
    }
    (tasks, true)
}

/// The name and the values one `taskParameterDefinitions` entry declares, or `None` when it
/// declares a space this reader does not enumerate.
fn parameter_values(definition: &Value) -> Option<(String, Vec<TaskParameterValue>)> {
    let name = definition.get("name")?.as_str()?;
    // The name is stored with every task of the step, so a name this long is a name whose
    // space this reader does not enumerate.
    if name.is_empty() || name.len() > MAX_PARAMETER_NAME {
        return None;
    }
    let kind = definition.get("type")?.as_str()?;
    let texts = match definition.get("range")? {
        Value::Array(items) => items.iter().map(scalar_text).collect::<Option<Vec<_>>>()?,
        Value::String(expression) if kind == PARAMETER_TYPE_INT => {
            int_range_expression(expression)?
        }
        _ => return None,
    };
    let values = texts
        .iter()
        .map(|text| task_parameter_value(kind, text))
        .collect::<Option<Vec<_>>>()?;
    Some((name.to_string(), values))
}

/// One declared range element as the text a `TaskParameterValue` carries.
///
/// Every member of that union is a STRING on the wire, so a range element declared as a
/// number is carried as the digits it was declared with.
fn scalar_text(element: &Value) -> Option<String> {
    match element {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// `text` as the `TaskParameterValue` member the declared `kind` names, when it satisfies
/// that member's own modeled form.
fn task_parameter_value(kind: &str, text: &str) -> Option<TaskParameterValue> {
    match kind {
        // `IntString` is `^[-]?(0|[1-9][0-9]*)$`, which is exactly the canonical decimal
        // form of an `i64`.
        PARAMETER_TYPE_INT => {
            let value: i64 = text.parse().ok()?;
            (value.to_string() == text).then(|| TaskParameterValue::Int(text.to_string()))
        }
        // `FloatString` is `^[-]?(0|[1-9][0-9]*)([.][0-9]+)?([eE][+-]?[0-9]+)?$`,
        // `len 1..26`: a decimal with an optional fraction and exponent, which is narrower
        // than what a float parser accepts (no `NaN`, no infinity, no leading `+`).
        PARAMETER_TYPE_FLOAT => (text.len() <= MAX_FLOAT_STRING && is_float_string(text))
            .then(|| TaskParameterValue::Float(text.to_string())),
        PARAMETER_TYPE_STRING => (text.len() <= MAX_PARAMETER_STRING)
            .then(|| TaskParameterValue::String(text.to_string())),
        PARAMETER_TYPE_PATH => {
            (text.len() <= MAX_PARAMETER_STRING).then(|| TaskParameterValue::Path(text.to_string()))
        }
        _ => None,
    }
}

/// Whether `text` is the decimal form `FloatString` spells:
/// `^[-]?(0|[1-9][0-9]*)([.][0-9]+)?([eE][+-]?[0-9]+)?$`.
fn is_float_string(text: &str) -> bool {
    let (mantissa, exponent) = match text.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, Some(exponent)),
        None => (text, None),
    };
    if let Some(exponent) = exponent {
        let digits = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
    }
    let (whole, fraction) = match mantissa.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (mantissa, None),
    };
    if let Some(fraction) = fraction {
        if fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
    }
    // The whole part is the same canonical decimal integer `IntString` spells.
    whole
        .parse::<i64>()
        .is_ok_and(|value| value.to_string() == whole)
}

/// The values an `INT` range expression spells out, or `None` for an expression this reader
/// does not enumerate.
///
/// The expression is comma-separated items, each a single value, a `<start>-<end>` interval
/// or a `<start>-<end>:<step>` interval over non-negative values — the form an Open Job
/// Description `INT` range is written in. A negative bound, a reference to be resolved
/// elsewhere (`{{...}}`), a step of zero and a descending interval are all `None`: enumerating
/// them is guesswork, and an unread space is reported as unread rather than filled in.
fn int_range_expression(expression: &str) -> Option<Vec<String>> {
    let mut values: Vec<String> = Vec::new();
    for item in expression.split(',') {
        let item = item.trim();
        let (bounds, step) = match item.split_once(':') {
            Some((bounds, step)) => (bounds.trim(), step.trim().parse::<i64>().ok()?),
            None => (item, 1),
        };
        if step <= 0 {
            return None;
        }
        let (start, end) = match bounds.split_once('-') {
            Some((start, end)) => (start.trim(), end.trim()),
            None => (bounds, bounds),
        };
        let start: i64 = start.parse().ok()?;
        let end: i64 = end.parse().ok()?;
        if start < 0 || end < start {
            return None;
        }
        let mut value = start;
        while value <= end {
            if values.len() >= MAX_TASKS_PER_STEP {
                return None;
            }
            values.push(value.to_string());
            // The interval is caller-supplied, so the step past the last value is the one
            // that can leave the range of the value itself: an interval that cannot be
            // walked to its end is one this reader has not read.
            match value.checked_add(step) {
                Some(next) => value = next,
                None => break,
            }
        }
    }
    (!values.is_empty()).then_some(values)
}

/// The longest owner id an [`environment_id`] embeds: a step's, which is a character
/// longer than a job's. The id a response names an environment by embeds its name, so a
/// name is only readable here when the longest id that could carry it still fits.
const LONGEST_ENVIRONMENT_OWNER: &str = "step-00000000000000000000000000000000";

/// The names an `environments` list declares, or `None` when one of them is a name the
/// `EnvironmentId` the API carries for it cannot hold.
fn environment_names(declared: Option<&Value>) -> Option<Vec<String>> {
    let declared = match declared {
        None | Some(Value::Null) => return Some(Vec::new()),
        Some(Value::Array(environments)) => environments,
        Some(_) => return None,
    };
    if declared.len() > MAX_TEMPLATE_ENVIRONMENTS {
        return None;
    }
    let mut names = Vec::new();
    for environment in declared {
        let name = environment.get("name")?.as_str()?;
        super::environment_id(
            super::ENVIRONMENT_SCOPE_STEP,
            LONGEST_ENVIRONMENT_OWNER,
            name,
        )?;
        names.push(name.to_string());
    }
    Some(names)
}
