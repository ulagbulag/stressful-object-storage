use std::{
    convert::identity,
    fmt::Write,
    sync::{
        atomic::{AtomicU64, AtomicU8, Ordering},
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
use s3::{serde_types::InitiateMultipartUploadResponse, Bucket, BucketConfiguration};
use tokio::{spawn, task::JoinHandle, time::sleep};
use tracing::{error, info, instrument, Level};

use crate::args::{Args, LoadTesterArgs, LoadTesterJobArgs, Mode};

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
            bucket_create,
            credentials,
            load_tester,
            load_tester_job,
            region,
        } = args;

        let mut bucket = Bucket::new(
            &bucket_name,
            region.clone().into(),
            credentials.clone().into(),
        )
        .map_err(|error| anyhow!("failed to initialize object storage bucket client: {error}"))?
        .with_path_style();

        if !check_bucket_exists(&bucket).await {
            if bucket_create {
                let config = BucketConfiguration::private();
                let response = Bucket::create_with_path_style(
                    &bucket_name,
                    region.into(),
                    credentials.into(),
                    config,
                )
                .await
                .map_err(|error| anyhow!("failed to create object storage bucket: {error}"))?;
                if response.success() {
                    bucket = response.bucket.with_path_style();
                } else {
                    bail!("failed to create bucket: {bucket_name}")
                }
            } else {
                bail!("no such bucket: {bucket_name}")
            }
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
                    mode,
                    no_progress_bar,
                    threads_max,
                },
        } = self;

        let duration = duration.map(Into::into);
        let counter = Arc::<AtomicU64>::default();
        let state = Arc::<AtomicU8>::default();

        let task_handler = (0..threads_max)
            .map(|id| SessionTask {
                args: args.clone(),
                bucket: bucket.clone(),
                counter: counter.clone(),
                duration,
                id,
                mode,
                signal: signal.clone(),
                state: state.clone(),
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
                multipart_threshold: _,
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

            while state.load(Ordering::SeqCst) != SessionTask::STATE_READE {
                sleep(Duration::from_millis(10)).await;
            }
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
                    break cleanup(bucket).await;
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
    mode: Mode,
    signal: FunctionSignal,
    state: Arc<AtomicU8>,
    total_tasks: usize,
}

impl SessionTask {
    const CONTENT_TYPE: &'static str = "application/octet-stream";

    const STATE_PENDING: u8 = 0;
    const STATE_INIT: u8 = 1;
    const STATE_READE: u8 = 2;

    async fn try_loop_forever(self) -> Result<()> {
        let Self {
            args:
                LoadTesterArgs {
                    count,
                    multipart_threshold: _,
                    size,
                    step,
                },
            bucket: _,
            counter: _,
            duration,
            id,
            mode,
            signal,
            state,
            total_tasks,
        } = &self;

        let count = count.map(|count| count.as_u64() as usize);
        let size = size.as_u64() as usize;
        let step = step.as_u64() as usize;

        info!("Creating buffer map: {id}/{total_tasks}");
        let mut buf = vec![0; size + step];
        {
            let mut rng = SmallRng::from_entropy();
            rng.fill_bytes(&mut buf);
        }

        if state
            .compare_exchange(
                Self::STATE_PENDING,
                Self::STATE_INIT,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
        {
            info!("Initializing mode: {mode:?}");
            match mode {
                Mode::Read => self.init_read(&buf).await?,
                Mode::Write => self.init_write().await?,
            }
            state.store(Self::STATE_READE, Ordering::SeqCst);
        } else {
            while state.load(Ordering::SeqCst) != Self::STATE_READE {
                sleep(Duration::from_millis(10)).await;
            }
        }

        info!("Starting task: {id}/{total_tasks}");

        let mut index = *id;
        let instant = Instant::now();

        loop {
            if signal.is_terminating() {
                break;
            }
            if let Some(count) = count {
                if index >= count {
                    break;
                }
            }
            if let Some(duration) = *duration {
                if instant.elapsed() >= duration {
                    break;
                }
            }

            let index = {
                let ret = index;
                index += *total_tasks;
                ret % step
            };
            match mode {
                Mode::Read => self.read(index, true).await?,
                Mode::Write => self.write(index, &buf, true).await?,
            }
        }

        info!("Stopped task: {id}/{total_tasks}");
        Ok(())
    }

    async fn init_read(&self, buf: &[u8]) -> Result<()> {
        let step = self.args.step.as_u64() as usize;

        for index in 0..step {
            self.write(index, buf, false).await?;
        }
        Ok(())
    }

    async fn init_write(&self) -> Result<()> {
        Ok(())
    }

    async fn read(&self, index: usize, add_counter: bool) -> Result<()> {
        let size = self.args.size.as_u64() as usize;

        let path = get_s3_path(index);

        let response = self.bucket.get_object(&path).await?;
        // assert_eq!(response.bytes().len(), size);
        drop(response);

        if add_counter {
            self.counter.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }

    async fn write(&self, index: usize, buf: &[u8], add_counter: bool) -> Result<()> {
        let multipart_minimal = LoadTesterArgs::minimal_multipart_threshold().as_u64() as usize;
        let multipart_threshold = self.args.multipart_threshold.as_u64() as usize;
        let size = self.args.size.as_u64() as usize;
        let use_multipart = size > multipart_threshold;

        let path = get_s3_path(index);

        let data = &buf[index..index + size];
        if use_multipart {
            let InitiateMultipartUploadResponse { upload_id, .. } = self
                .bucket
                .initiate_multipart_upload(&path, Self::CONTENT_TYPE)
                .await?;

            let mut chunks = vec![];
            {
                let mut pos = 0;
                let len = data.len();
                while pos < len {
                    let pos_next = pos + multipart_threshold;
                    let remaining = len - pos_next;

                    let pos_next = if remaining >= multipart_minimal {
                        pos_next
                    } else {
                        len
                    };

                    let chunk = &data[pos..pos_next];
                    chunks.push(chunk);
                    pos = pos_next;
                }
            }

            let mut parts = vec![];
            for (part_number, reader) in chunks.iter_mut().enumerate() {
                let part_number = (part_number + 1).try_into()?;

                let part = self
                    .bucket
                    .put_multipart_stream(
                        reader,
                        &path,
                        part_number,
                        &upload_id,
                        Self::CONTENT_TYPE,
                    )
                    .await?;
                parts.push(part);
            }

            self.bucket
                .complete_multipart_upload(&path, &upload_id, parts)
                .await?;
        } else {
            let mut reader = data;
            self.bucket.put_object_stream(&mut reader, &path).await?;
        }

        if add_counter {
            self.counter.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

async fn check_bucket_exists(bucket: &Bucket) -> bool {
    match try_check_bucket_exists(bucket).await {
        Ok(_) => true,
        Err(error) => false,
    }
}

#[instrument(skip_all, err(level = Level::ERROR))]
async fn try_check_bucket_exists(bucket: &Bucket) -> Result<()> {
    const TEST_FILE: &'static str = "/_sos_bucket_test";

    bucket.put_object(TEST_FILE, TEST_FILE.as_bytes()).await?;
    bucket.delete_object(TEST_FILE).await.ok();
    Ok(())
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

fn get_s3_path(index: usize) -> String {
    format!("/sample/{index:06}.bin")
}
