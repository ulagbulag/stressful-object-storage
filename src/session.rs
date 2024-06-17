use std::{
    convert::identity,
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Result};
use ark_core::signal::FunctionSignal;
use clap::Parser;
use futures::{stream::FuturesUnordered, FutureExt, TryFutureExt, TryStreamExt};
use rand::{rngs::SmallRng, RngCore, SeedableRng};
use s3::Bucket;
use tokio::{spawn, task::JoinHandle};
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
                    threads_max,
                },
        } = self;

        let duration = duration.map(Into::into);

        (0..threads_max)
            .map(|id| SessionTask {
                args: args.clone(),
                bucket: bucket.clone(),
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
            .try_collect()
            .and_then(|()| cleanup(bucket))
            .await
    }
}

struct SessionTask {
    args: LoadTesterArgs,
    bucket: Bucket,
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
            duration,
            id,
            signal,
            total_tasks,
        } = self;

        let instant = Instant::now();

        let mut rng = SmallRng::from_entropy();
        let count = count.map(|count| count.as_u64() as usize);
        let size = size.as_u64() as usize;
        let step = step.as_u64() as usize;

        let mut buf = vec![0; size];
        let mut index = id;

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

            rng.fill_bytes(&mut buf);
            bucket.put_object(&path, &buf).await?;
        }
        Ok(())
    }
}

async fn cleanup(bucket: Bucket) -> Result<()> {
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
