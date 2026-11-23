// SPDX-License-Identifier: CC0-1.0

//! An ARRAY job's children, probed end to end over the assembled emulator: every child runs
//! the one program its definition declares, and what tells one child's work from another's is
//! the variables AWS Batch sets in its container.
//!
//! The RULE under test: **AWS Batch SETS variables of its own in the container of every job it
//! runs, the index of an array child among them, so each child of one program does its own
//! shard of the work and the parent's `statusSummary` counts them all.** The member that
//! carries the variables a definition declares reserves the prefix for the service —
//! "environment variables cannot start with `AWS_BATCH`. This naming convention is reserved
//! for variables that Batch sets" (`smithy_query(query="shape", shape="ContainerProperties")`,
//! member `environment`) — and the AWS Batch User Guide names the ones it sets for a
//! single-node container job: the job, the attempt, the queue, the compute environment, and
//! the array index of a child, which `ArrayPropertiesDetail$index` ("the job index within the
//! array that's associated with this job") reports for the same job.
//!
//! Why this suite lives beside the router, and why it drives the harness of
//! [`super::batch_job_container_runs_its_images_entry_point`] rather than a second copy of it:
//! the array's children are Batch's, the containers that run them are Amazon ECS's, the role
//! their calls are made as is IAM's, the objects they write are Amazon S3's and the lines they
//! wrote are CloudWatch Logs' — the same estate that sibling builds, which is what both halves
//! of one rule need. Everything asserted is read back through a real read: the parent and its
//! children through `DescribeJobs`, the objects through Amazon S3's own `GetObject`, and the
//! lines through `GetLogEvents`.
//!
//! The halves differ along what the rule generalises over: an array job whose children key the
//! object each writes off its OWN index (so what the array leaves behind is one object per
//! child, and none at the key an absent index would leave), an array job whose children all
//! run the same transfer on capacity that cannot place them at once (so children that WAIT are
//! covered as well as the first wave), and a job that is not an array child at all, which
//! reads the variables every job gets and reports them — each asserted against the value this
//! emulator's own state holds for it, never against a literal.

use super::batch_job_container_runs_its_images_entry_point::{
    described, emulator, given_the_account, given_the_queue, held_object, lines_until_written,
    register, submit, until_terminal, BUCKET, DESTINATION_PREFIX, FAILED, PAYLOAD, REGION,
    SHELL_IMAGE, SOURCE_KEY, SUCCEEDED,
};

/// The AWS CLI's official image, whose own entry point is the CLI, so a definition of it
/// carries the CLI's arguments as its `command`.
const GALLERY_CLI_IMAGE: &str = "public.ecr.aws/aws-cli/aws-cli:2.36.44";

/// How many children the index-keyed array job below fans out to.
const SHARDS: i64 = 4;

/// How many children the second array job fans out to, and how many vCPUs its capacity holds:
/// enough children that the queue cannot place them all at once, so the rule is exercised over
/// children that WAIT for capacity as well as over the first wave.
const ARRAY_SIZE: i64 = 8;
const ARRAY_MAX_VCPUS: i64 = 1;

/// The vCPU reservation every job definition below states — the smallest Fargate size.
const VCPUS: &str = "0.25";

/// RULE: the variables AWS Batch sets in a job's container are there to be read — the job, the
/// attempt, the queue, the compute environment, and, for a child of an array job, the index of
/// the shard it owns, which is what makes every child of one program do its own work.
#[tokio::test]
async fn the_variables_batch_sets_are_in_a_jobs_container() {
    let case = "the variables Batch sets";
    let (router, engine) = emulator();
    let account = given_the_account(&router, &engine, case).await;
    let queue = given_the_queue(&router, case, &account, "batch-vars", 16).await;

    // The SHARDED array job: ONE program, and the index its own container reads decides both
    // the file each child writes and the object it carries that file to.
    let sharded = register(
        &router,
        case,
        &account,
        "batch-vars-sharded",
        SHELL_IMAGE,
        &[
            "sh".to_string(),
            "-c".to_string(),
            format!(
                "echo shard-$AWS_BATCH_JOB_ARRAY_INDEX > /tmp/shard.txt && \
                 aws s3 cp /tmp/shard.txt \
                 s3://{BUCKET}/{DESTINATION_PREFIX}/shard-$AWS_BATCH_JOB_ARRAY_INDEX \
                 --region {REGION}"
            ),
        ],
        VCPUS,
    )
    .await;
    // The job that reports the variables every job gets, on its own standard output.
    let reporter = register(
        &router,
        case,
        &account,
        "batch-vars-reporter",
        SHELL_IMAGE,
        &[
            "sh".to_string(),
            "-c".to_string(),
            "echo JOB:$AWS_BATCH_JOB_ID; echo ATTEMPT:$AWS_BATCH_JOB_ATTEMPT; \
             echo QUEUE:$AWS_BATCH_JQ_NAME; echo ENVIRONMENT:$AWS_BATCH_CE_NAME; \
             echo INDEX:[$AWS_BATCH_JOB_ARRAY_INDEX]"
                .to_string(),
        ],
        VCPUS,
    )
    .await;
    let parent = submit(
        &router,
        case,
        &queue,
        &sharded,
        "batch-vars-array-job",
        Some(SHARDS),
    )
    .await;
    let reporting = submit(&router, case, &queue, &reporter, "batch-vars-job", None).await;

    let detail = until_terminal(&router, case, &parent).await;
    let summary = detail["arrayProperties"]["statusSummary"].clone();
    assert_eq!(
        summary[SUCCEEDED].as_i64(),
        Some(SHARDS),
        "case {case:?}: expected every shard to succeed, got {summary} on {detail}"
    );
    // ONE object per child, each at the key its own index named, and each holding the bytes its
    // transfer carried: the index is what told the children apart.
    for index in 0..SHARDS {
        let key = format!("{DESTINATION_PREFIX}/shard-{index}");
        let held = held_object(&router, &key).await;
        assert_eq!(
            held.as_deref().map(str::trim),
            Some(format!("shard-{index}").as_str()),
            "case {case:?}: expected the object child {index} wrote at {key}, got {held:?}"
        );
    }
    // And no child wrote the key an ABSENT index would have left: an empty expansion would
    // have made every child address one and the same object.
    let unsharded = held_object(&router, &format!("{DESTINATION_PREFIX}/shard-")).await;
    assert!(
        unsharded.is_none(),
        "case {case:?}: expected no object at the key an absent index would leave, got \
         {unsharded:?}"
    );

    // The variables every job gets, read back off the lines the container wrote and compared
    // with what this emulator holds for that job.
    let reported = until_terminal(&router, case, &reporting).await;
    assert_eq!(
        reported["status"].as_str(),
        Some(SUCCEEDED),
        "case {case:?}: expected the reporting job to succeed, got {reported}"
    );
    let lines = lines_until_written(&router, case, &reported, "INDEX").await;
    let value = |name: &str| {
        lines
            .iter()
            .find_map(|line| line.strip_prefix(&format!("{name}:")))
            .map(str::to_string)
    };
    for (name, expected) in [
        ("JOB", reporting.clone()),
        ("ATTEMPT", "1".to_string()),
        ("QUEUE", queue.clone()),
        ("ENVIRONMENT", "batch-vars-ce".to_string()),
        // A job that is not an array child has no index, so nothing is set for it — the
        // brackets are what make an empty expansion visible.
        ("INDEX", "[]".to_string()),
    ] {
        assert_eq!(
            value(name).as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected the container to read {name} as {expected:?}, got {lines:?}"
        );
    }
}

/// RULE, over the children that WAIT: every child of an array job runs the program its
/// definition declares — the image's entry point with that command as its arguments — so each
/// child makes its transfer and the parent's `statusSummary` counts them all as `SUCCEEDED`,
/// however few of them the capacity behind the queue can place at once.
#[tokio::test]
async fn every_child_of_an_array_job_runs_that_program_too() {
    let case = "an array job of the CLI's own image";
    let (router, engine) = emulator();
    let account = given_the_account(&router, &engine, case).await;
    let queue = given_the_queue(&router, case, &account, "batch-cli-array", ARRAY_MAX_VCPUS).await;
    let command = vec![
        "s3".to_string(),
        "cp".to_string(),
        format!("s3://{BUCKET}/{SOURCE_KEY}"),
        format!("s3://{BUCKET}/{DESTINATION_PREFIX}/array.txt"),
        "--region".to_string(),
        REGION.to_string(),
    ];
    let definition = register(
        &router,
        case,
        &account,
        "batch-cli-array-definition",
        GALLERY_CLI_IMAGE,
        &command,
        VCPUS,
    )
    .await;
    let parent = submit(
        &router,
        case,
        &queue,
        &definition,
        "batch-cli-array-job",
        Some(ARRAY_SIZE),
    )
    .await;

    // The parent settles when its children do, and what it settles on is what they add up to.
    let detail = until_terminal(&router, case, &parent).await;
    let summary = detail["arrayProperties"]["statusSummary"].clone();
    assert_eq!(
        summary[SUCCEEDED].as_i64(),
        Some(ARRAY_SIZE),
        "case {case:?}: expected every child to succeed, got {summary} on {detail}"
    );
    assert_eq!(
        summary[FAILED].as_i64(),
        Some(0),
        "case {case:?}: expected no failed child, got {summary} on {detail}"
    );
    assert_eq!(
        detail["status"].as_str(),
        Some(SUCCEEDED),
        "case {case:?}: expected the parent to succeed with its children, got {detail}"
    );

    // Each child really ran the program: its own container reports the status that program
    // exited with, and the object the transfers carried holds the source bytes.
    for index in 0..ARRAY_SIZE {
        let child = described(&router, case, &format!("{parent}:{index}")).await;
        assert_eq!(
            child["status"].as_str(),
            Some(SUCCEEDED),
            "case {case:?}: expected child {index} to succeed, got {child}"
        );
        assert_eq!(
            child["container"]["exitCode"].as_i64(),
            Some(0),
            "case {case:?}: expected child {index} to report the status its program exited \
             with, got {child}"
        );
    }
    let held = held_object(&router, &format!("{DESTINATION_PREFIX}/array.txt")).await;
    assert_eq!(
        held.as_deref(),
        Some(PAYLOAD),
        "case {case:?}: expected the object the children carried, got {held:?}"
    );
}
