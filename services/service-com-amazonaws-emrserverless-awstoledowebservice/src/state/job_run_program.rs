// SPDX-License-Identifier: CC0-1.0

//! The WORKER a job run's driver ran as, and what a run that terminated CONSUMED over the
//! span it executed for.
//!
//! A job run reports what it took once it has come to rest: `totalResourceUtilization` is "the
//! aggregate vCPU, memory, and storage resources used from the time the job starts to execute,
//! until the time the job terminates, rounded up to the nearest second" and
//! `billedResourceUtilization` is "the aggregate vCPU, memory, and storage that Amazon Web
//! Services has billed for the job run. The billed resources include a 1-minute minimum usage
//! for workers, plus additional storage over 20 GB per worker". Both are a DURATION times the
//! resources the run's worker held, so this module is the second half of that product: the
//! worker, read off the submit's own `sparkSubmitParameters`, and the arithmetic the two
//! members document.
//!
//! The duration itself is the run's ([`super::JobRunRecord::settle`]), and WHAT ran over it is
//! [`super::spark_job_run`]'s: a run that never moved to `RUNNING` executed nothing and
//! reports no utilisation at all.

use super::*;

/// The worker a job run's DRIVER ran as: the resources one worker holds while the run is
/// executing, which is what its resource utilisation is measured over.
///
/// EMR Serverless sizes a Spark driver through the submit's own `sparkSubmitParameters`
/// (`spark.driver.cores`, `spark.driver.memory`, `spark.emr-serverless.driver.disk`), which is
/// where these come from when the submit names them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DriverWorker {
    /// vCPU the worker holds.
    pub v_cpu: f64,
    /// Memory it holds, in GB.
    pub memory_gb: f64,
    /// Ephemeral storage it holds, in GB.
    pub storage_gb: f64,
}

/// The vCPU this emulator attributes a driver whose submit sized none.
///
/// Source: the worker sizes EMR Serverless documents for a Spark driver that configures none
/// — `spark.driver.cores=4`, `spark.driver.memory=14g`, `spark.emr-serverless.driver.disk=20g`
/// — so a run that sized nothing is measured over the worker the service would have given it.
/// A submit that sizes its driver is measured at the size it asked for.
pub const DEFAULT_DRIVER_V_CPU: f64 = 4.0;

/// The memory this emulator attributes such a driver, from the same documented defaults.
pub const DEFAULT_DRIVER_MEMORY_GB: f64 = 14.0;

/// The ephemeral storage this emulator attributes such a driver, from the same documented
/// defaults — and the per-worker storage the billing rule names: "The billed resources include
/// a 1-minute minimum usage for workers, plus additional storage over 20 GB per worker"
/// (`JobRun$billedResourceUtilization`), so a worker of this size is billed for no storage at
/// all and only storage a submit asked for ABOVE it is.
pub const DEFAULT_DRIVER_STORAGE_GB: f64 = 20.0;

/// The storage per worker that is not billed, from the same sentence.
pub const FREE_STORAGE_GB_PER_WORKER: f64 = 20.0;

/// The shortest usage a worker is billed for, from the same sentence ("a 1-minute minimum
/// usage for workers").
pub const BILLED_MINIMUM: Duration = Duration::from_secs(60);

/// How many workers a run of this emulator holds: the DRIVER that ran the program, in this
/// process. Nothing schedules an executor here — the Spark runtime computes a program's
/// partitions in the driver itself — so one worker is what a run really used.
pub const WORKERS_PER_RUN: f64 = 1.0;

impl Default for DriverWorker {
    fn default() -> Self {
        Self {
            v_cpu: DEFAULT_DRIVER_V_CPU,
            memory_gb: DEFAULT_DRIVER_MEMORY_GB,
            storage_gb: DEFAULT_DRIVER_STORAGE_GB,
        }
    }
}

/// Seconds in one hour, the unit every utilisation member is reported in (`vCPUHour`,
/// `memoryGBHour`, `storageGBHour`).
const SECONDS_PER_HOUR: f64 = 3600.0;

impl DriverWorker {
    /// The worker the driver of a job run with these `sparkSubmitParameters` ran as.
    ///
    /// Each member the parameters size is taken from them and the rest stay the sizes this
    /// emulator attributes a driver that was not sized ([`DriverWorker::default`]).
    #[must_use]
    pub fn of(spark_submit_parameters: Option<&str>) -> Self {
        let mut worker = Self::default();
        let Some(parameters) = spark_submit_parameters else {
            return worker;
        };
        if let Some(cores) = configured(parameters, DRIVER_CORES_PROPERTY).and_then(parse_count) {
            worker.v_cpu = cores;
        }
        if let Some(memory) = configured(parameters, DRIVER_MEMORY_PROPERTY).and_then(parse_size_gb)
        {
            worker.memory_gb = memory;
        }
        if let Some(disk) = configured(parameters, DRIVER_DISK_PROPERTY).and_then(parse_size_gb) {
            worker.storage_gb = disk;
        }
        worker
    }

    /// What a run of `executed` really consumed, as `TotalResourceUtilization` reports it:
    /// "the aggregate vCPU, memory, and storage resources used from the time job start
    /// executing till the time job is terminated, rounded up to the nearest second".
    #[must_use]
    pub fn total_utilization(&self, executed: Duration) -> TotalResourceUtilization {
        let hours = self.hours(executed);
        TotalResourceUtilization {
            v_cpu_hour: Some(self.v_cpu * WORKERS_PER_RUN * hours),
            memory_gb_hour: Some(self.memory_gb * WORKERS_PER_RUN * hours),
            storage_gb_hour: Some(self.storage_gb * WORKERS_PER_RUN * hours),
        }
    }

    /// What is BILLED for that run: "The billed resources include a 1-minute minimum usage for
    /// workers, plus additional storage over 20 GB per worker"
    /// (`JobRun$billedResourceUtilization`) — so the span is held to [`BILLED_MINIMUM`] and the
    /// storage billed is only what the worker holds above [`FREE_STORAGE_GB_PER_WORKER`].
    #[must_use]
    pub fn billed_utilization(&self, executed: Duration) -> ResourceUtilization {
        let hours = self.hours(executed.max(BILLED_MINIMUM));
        let billed_storage = (self.storage_gb - FREE_STORAGE_GB_PER_WORKER).max(0.0);
        ResourceUtilization {
            v_cpu_hour: Some(self.v_cpu * WORKERS_PER_RUN * hours),
            memory_gb_hour: Some(self.memory_gb * WORKERS_PER_RUN * hours),
            storage_gb_hour: Some(billed_storage * WORKERS_PER_RUN * hours),
        }
    }

    /// The span `executed`, in hours, with the span itself first rounded UP to the nearest
    /// second — the rounding `TotalResourceUtilization` documents.
    fn hours(&self, executed: Duration) -> f64 {
        let seconds = executed_seconds(executed);
        f64::from(seconds) / SECONDS_PER_HOUR
    }
}

/// The span `executed`, in whole seconds rounded UP, which is both what
/// `totalExecutionDurationSeconds` reports and the unit utilisation is aggregated over
/// ("rounded up to the nearest second").
///
/// Held inside `Integer`, the type `JobRun$totalExecutionDurationSeconds` targets, so a span no
/// `i32` can hold is reported as the largest one it can rather than wrapping.
#[must_use]
pub fn executed_seconds(executed: Duration) -> i32 {
    let seconds = executed.as_secs() + u64::from(executed.subsec_nanos() > 0);
    i32::try_from(seconds).unwrap_or(i32::MAX)
}

/// `spark.driver.cores`: "Number of cores to use for the driver process."
const DRIVER_CORES_PROPERTY: &str = "spark.driver.cores";

/// `spark.driver.memory`: the memory the driver process holds.
const DRIVER_MEMORY_PROPERTY: &str = "spark.driver.memory";

/// `spark.emr-serverless.driver.disk`: the ephemeral storage the driver worker holds, which is
/// the member the storage billing rule above is stated per worker of.
const DRIVER_DISK_PROPERTY: &str = "spark.emr-serverless.driver.disk";

/// The value `property` is set to in `parameters`, or `None` when it sets it nowhere.
///
/// `sparkSubmitParameters` is a command line, and a property is set in it as
/// `--conf <property>=<value>`; the LAST setting wins, as it does for `spark-submit` itself.
fn configured(parameters: &str, property: &str) -> Option<String> {
    let prefix = format!("{property}=");
    parameters
        .split_whitespace()
        .filter_map(|word| word.strip_prefix(&prefix))
        .next_back()
        .map(str::to_string)
}

/// A bare count (`spark.driver.cores=4`), as a positive number of units.
fn parse_count(value: String) -> Option<f64> {
    let parsed: f64 = value.trim().parse().ok()?;
    (parsed.is_finite() && parsed > 0.0).then_some(parsed)
}

/// A Spark memory or disk size (`14g`, `2048m`, `20GB`), in GB.
///
/// Spark writes such a size as a number with a unit suffix, and a value with no suffix at all
/// is read as mebibytes — the unit `spark-submit` reads a bare JVM memory size in.
fn parse_size_gb(value: String) -> Option<f64> {
    let text = value.trim().to_ascii_lowercase();
    let text = text.strip_suffix('b').unwrap_or(&text);
    let (digits, per_gb) = match text.strip_suffix('g') {
        Some(digits) => (digits, 1.0),
        None => match text.strip_suffix('m') {
            Some(digits) => (digits, 1.0 / 1024.0),
            None => match text.strip_suffix('k') {
                Some(digits) => (digits, 1.0 / (1024.0 * 1024.0)),
                None => match text.strip_suffix('t') {
                    Some(digits) => (digits, 1024.0),
                    // No unit: mebibytes.
                    None => (text, 1.0 / 1024.0),
                },
            },
        },
    };
    let parsed: f64 = digits.trim().parse().ok()?;
    (parsed.is_finite() && parsed > 0.0).then_some(parsed * per_gb)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the driver worker a run is measured over is the one the submit's own
    /// `sparkSubmitParameters` size, and the sizes this emulator attributes a driver that was
    /// not sized otherwise.
    #[test]
    fn the_driver_worker_is_the_size_the_submit_asked_for() {
        assert_eq!(
            DriverWorker::of(None),
            DriverWorker::default(),
            "case no parameters: expected the attributed size"
        );
        let sized = DriverWorker::of(Some(
            "--conf spark.driver.cores=2 --conf spark.driver.memory=8g --conf \
             spark.emr-serverless.driver.disk=50g",
        ));
        assert_eq!(
            sized,
            DriverWorker {
                v_cpu: 2.0,
                memory_gb: 8.0,
                storage_gb: 50.0,
            },
            "case a sized driver: expected the sizes the submit named, got {sized:?}"
        );
        let in_mebibytes = DriverWorker::of(Some("--conf spark.driver.memory=2048m"));
        assert!(
            (in_mebibytes.memory_gb - 2.0).abs() < f64::EPSILON,
            "case a size in mebibytes: expected 2 GB, got {:?}",
            in_mebibytes.memory_gb
        );
        let last_wins = DriverWorker::of(Some(
            "--conf spark.driver.cores=1 --conf spark.driver.cores=3",
        ));
        assert!(
            (last_wins.v_cpu - 3.0).abs() < f64::EPSILON,
            "case a property set twice: expected the last setting, got {:?}",
            last_wins.v_cpu
        );
    }

    /// RULE: what a run consumed is its own span times the worker it ran as, and what is
    /// billed holds that span to the documented one-minute minimum and bills only storage
    /// above the documented 20 GB per worker.
    #[test]
    fn utilisation_is_measured_over_the_span_the_run_took() {
        let worker = DriverWorker {
            v_cpu: 2.0,
            memory_gb: 8.0,
            storage_gb: 20.0,
        };
        let total = worker.total_utilization(Duration::from_secs(1800));
        assert_eq!(
            total.v_cpu_hour,
            Some(1.0),
            "case half an hour on two vCPU: expected one vCPU-hour, got {:?}",
            total.v_cpu_hour
        );
        assert_eq!(
            total.storage_gb_hour,
            Some(10.0),
            "case half an hour on 20 GB: expected ten GB-hours, got {:?}",
            total.storage_gb_hour
        );
        let billed = worker.billed_utilization(Duration::from_secs(1));
        assert_eq!(
            billed.v_cpu_hour,
            Some(2.0 / 60.0),
            "case a second of work: expected the one-minute minimum billed, got {:?}",
            billed.v_cpu_hour
        );
        assert_eq!(
            billed.storage_gb_hour,
            Some(0.0),
            "case a worker at the free storage size: expected no storage billed, got {:?}",
            billed.storage_gb_hour
        );
        assert_eq!(
            executed_seconds(Duration::from_millis(1500)),
            2,
            "case a span of one and a half seconds: expected it rounded up"
        );
    }
}
