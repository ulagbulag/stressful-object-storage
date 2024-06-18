use std::{
    convert::identity,
    fmt::Write,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Result};
use ark_core::signal::FunctionSignal;
use byte_unit::{Byte, UnitType};
use clap::Parser;
use futures::{stream::FuturesUnordered, FutureExt, TryFutureExt, TryStreamExt};
use indicatif::{ProgressBar, ProgressState, ProgressStyle};
use rand::{rngs::SmallRng, RngCore, SeedableRng};
use s3::Bucket;
use tokio::{spawn, task::JoinHandle, time::sleep};
use tracing::{error, info};

use crate::args::{Args, LoadTesterArgs, LoadTesterJobArgs};

pub struct ObjectStorageSession {
    bucket: Bucket,
    load_tester: LoadTesterArgs,
    load_tester_job: LoadTesterJobArgs,
}

impl ObjectStorageSession {
    pub async fn try_default() -> Result<Self> {
        ::dotenv::dotenv().ok();
        let args = Args::try_parse()?;
        args.print();

        let Args {
            bucket_name,
            credentials,
            load_tester,
            load_tester_job,
            region,
        } = args;

        let bucket = Bucket::new(&bucket_name, region.into(), credentials.into())
            .map_err(|error| anyhow!("failed to initialize object storage bucket client: {error}"))?
            .with_path_style();

        if !bucket
            .exists()
            .await
            .map_err(|error| anyhow!("failed to check object storage bucket: {error}"))?
        {
            bail!("no such bucket: {bucket_name}")
        }

        Ok(Self {
            bucket,
            load_tester,
            load_tester_job,
        })
    }

    pub fn spawn(self, signal: FunctionSignal) -> JoinHandle<()> {
        spawn(self.loop_forever(signal))
    }

    async fn loop_forever(self, signal: FunctionSignal) {
        match self.try_loop_forever(signal.clone()).await {
            Ok(()) => signal.terminate(),
            Err(error) => {
                error!("{error}");
                signal.terminate_on_panic()
            }
        }
    }

    async fn try_loop_forever(self, signal: FunctionSignal) -> Result<()> {
        let Self {
            bucket,
            load_tester: args,
            load_tester_job:
                LoadTesterJobArgs {
                    duration,
                    no_progress_bar,
                    threads_max,
                },
        } = self;

        let duration = duration.map(Into::into);
        let counter = Arc::<AtomicU64>::default();

        let task_handler = (0..threads_max)
            .map(|id| SessionTask {
                args: args.clone(),
                bucket: bucket.clone(),
                counter: counter.clone(),
                duration,
                id,
                signal: signal.clone(),
                total_tasks: threads_max,
            })
            .map(|task| {
                spawn(task.try_loop_forever())
                    .map(|result| result.map_err(Into::into).and_then(identity))
            })
            .collect::<FuturesUnordered<_>>()
            .try_collect();

        if no_progress_bar {
            task_handler.await
        } else {
            let LoadTesterArgs {
                count,
                size,
                step: _,
            } = args;

            fn write_eta(state: &ProgressState, w: &mut dyn Write) {
                let eta = state.eta().as_secs_f64();
                write!(w, "{eta:.1}s").unwrap()
            }

            fn write_speed(state: &ProgressState, w: &mut dyn Write) {
                match Byte::from_f64(state.per_sec())
                    .map(|byte| byte.get_appropriate_unit(UnitType::Decimal))
                {
                    Some(byte) => write!(w, "{byte:.1}/s").unwrap(),
                    None => write!(w, "UNK").unwrap(),
                }
            }

            let pb = match count {
                Some(count) => ProgressBar::new(count.as_u64() * size.as_u64()),
                None => ProgressBar::new_spinner(),
            };

            let style = ProgressStyle::with_template(
                    "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta} | {speed})"
                )?
                .with_key("eta", write_eta)
                .with_key("speed", write_speed)
                .progress_chars("#>-");
            pb.set_style(style);

            loop {
                let progressed = counter.load(Ordering::SeqCst);
                pb.set_position(progressed * size.as_u64());

                let is_finished = count
                    .as_ref()
                    .map(|count| count.as_u64() == progressed)
                    .unwrap_or_default();
                if is_finished || signal.is_terminating() {
                    if is_finished {
                        pb.finish();
                    }
                    task_handler.await?;
                    break cleanup(bucket, signal).await;
                }

                sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

struct SessionTask {
    args: LoadTesterArgs,
    bucket: Bucket,
    counter: Arc<AtomicU64>,
    duration: Option<Duration>,
    id: usize,
    signal: FunctionSignal,
    total_tasks: usize,
}

impl SessionTask {
    async fn try_loop_forever(self) -> Result<()> {
        let Self {
            args: LoadTesterArgs { count, size, step },
            bucket,
            counter,
            duration,
            id,
            signal,
            total_tasks,
        } = self;

        let instant = Instant::now();

        let count = count.map(|count| count.as_u64() as usize);
        let size = size.as_u64() as usize;
        let step = step.as_u64() as usize;

        info!("Creating buffer map: {id}/{total_tasks}");
        let mut buf = vec![0; size + step];
        {
            let mut rng = SmallRng::from_entropy();
            rng.fill_bytes(&mut buf);
        }

        let mut index = id;

        info!("Starting task: {id}/{total_tasks}");
        loop {
            if signal.is_terminating() {
                break;
            }
            if let Some(count) = count {
                if index >= count {
                    break;
                }
            }
            if let Some(duration) = duration {
                if instant.elapsed() >= duration {
                    break;
                }
            }

            let index = {
                let ret = index;
                index += total_tasks;
                ret % step
            };
            let path = format!("/sample/{index:06}.bin");

            bucket.put_object(&path, &buf[index..index + size]).await?;
            counter.fetch_add(1, Ordering::SeqCst);
        }

        info!("Stopped task: {id}/{total_tasks}");
        Ok(())
    }
}

async fn cleanup(bucket: Bucket, signal: FunctionSignal) -> Result<()> {
    info!("Cleaning up...");

    let delimeter = "/".into();
    let prefix = "/sample/".into();
    let files = bucket
        .list(prefix, Some(delimeter))
        .await
        .map_err(|error| anyhow!("failed to validate bucket files: {error}"))?;

    files
        .into_iter()
        .map(|file| file.name)
        .map(|path| bucket.delete_object(path).map_ok(|_| ()))
        .collect::<FuturesUnordered<_>>()
        .try_collect()
        .map_err(|error| anyhow!("failed to cleanup bucket: {error}"))
        .await

    // bucket
    //     .delete_object("/sample/")
    //     .map_ok(|_| ())
    //     .map_err(|error| anyhow!("failed to cleanup bucket: {error}"))
    //     .await
}
