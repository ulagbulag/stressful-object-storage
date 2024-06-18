use std::fmt;

use byte_unit::Byte;
use clap::{ArgAction, Parser};
use duration_string::DurationString;
use s3::{creds::Credentials, Region};
use serde::{Deserialize, Serialize};
use tracing::info;

#[derive(Clone, Debug, PartialEq, Parser, Serialize, Deserialize)]
#[clap(rename_all = "kebab-case")]
#[serde(rename_all = "camelCase")]
pub struct Args {
    #[arg(long, env = "AWS_BUCKET", value_name = "NAME")]
    pub bucket_name: String,

    #[command(flatten)]
    #[serde(default, flatten)]
    pub credentials: CredentialsArgs,

    #[command(flatten)]
    #[serde(default, flatten)]
    pub load_tester: LoadTesterArgs,

    #[command(flatten)]
    #[serde(default, flatten)]
    pub load_tester_job: LoadTesterJobArgs,

    #[command(flatten)]
    #[serde(default, flatten)]
    pub region: RegionArgs,
}

impl Args {
    pub fn print(&self) {
        let Self {
            bucket_name,
            credentials,
            load_tester,
            load_tester_job,
            region,
        } = self;

        info!("bucket_name: {bucket_name}");
        credentials.print();
        load_tester.print();
        load_tester_job.print();
        region.print();
    }
}

#[derive(Clone, Default, PartialEq, Parser, Serialize, Deserialize)]
#[clap(rename_all = "kebab-case")]
#[serde(rename_all = "camelCase")]
pub struct CredentialsArgs {
    #[arg(long, env = "AWS_ACCESS_KEY_ID", value_name = "PLAIN")]
    #[serde(default)]
    pub access_key: Option<String>,

    #[arg(long, env = "AWS_SECRET_ACCESS_KEY", value_name = "PLAIN")]
    #[serde(default)]
    pub secret_key: Option<String>,

    #[arg(long, env = "AWS_SECURITY_TOKEN", value_name = "PLAIN")]
    #[serde(default)]
    pub security_token: Option<String>,

    #[arg(long, env = "AWS_SESSION_TOKEN", value_name = "PLAIN")]
    #[serde(default)]
    pub session_token: Option<String>,
}

impl From<CredentialsArgs> for Credentials {
    fn from(value: CredentialsArgs) -> Self {
        let CredentialsArgs {
            access_key,
            secret_key,
            security_token,
            session_token,
        } = value;
        Self {
            access_key,
            expiration: None,
            secret_key,
            security_token,
            session_token,
        }
    }
}

impl fmt::Debug for CredentialsArgs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = &"(hidden)" as &dyn fmt::Debug;

        f.debug_struct("CredentialsArgs")
            .field("access_key", value)
            .field("secret_key", value)
            .field("security_token", value)
            .field("session_token", value)
            .finish()
    }
}

impl CredentialsArgs {
    #[inline]
    const fn print(&self) {}
}

#[derive(Clone, Debug, PartialEq, Parser, Serialize, Deserialize)]
#[clap(rename_all = "kebab-case")]
#[serde(rename_all = "camelCase")]
pub struct LoadTesterArgs {
    #[arg(long, env = "SOS_COUNT", value_name = "NUM")]
    #[serde(default)]
    pub count: Option<Byte>,

    #[arg(long, env = "SOS_SIZE", value_name = "BYTES", default_value_t = LoadTesterArgs::default_size())]
    #[serde(default = "LoadTesterArgs::default_size")]
    pub size: Byte,

    #[arg(long, env = "SOS_STEP", value_name = "NUM", default_value_t = LoadTesterArgs::default_step())]
    #[serde(default = "LoadTesterArgs::default_step")]
    pub step: Byte,
}

impl Default for LoadTesterArgs {
    fn default() -> Self {
        Self {
            count: None,
            size: Self::default_size(),
            step: Self::default_step(),
        }
    }
}

impl LoadTesterArgs {
    const fn default_size() -> Byte {
        Byte::from_u64(4_000_000) // 4MB
    }

    const fn default_step() -> Byte {
        Byte::from_u64(64)
    }

    fn print(&self) {
        let Self { count, size, step } = self;

        info!(
            "count: {count}",
            count = count
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "None".into(),)
        );
        info!("size: {size}");
        info!("step: {step}");
    }
}

#[derive(Clone, Debug, PartialEq, Parser, Serialize, Deserialize)]
#[clap(rename_all = "kebab-case")]
#[serde(rename_all = "camelCase")]
pub struct LoadTesterJobArgs {
    #[arg(long, env = "SOS_DURATION", value_name = "DURATION")]
    #[serde(default)]
    pub duration: Option<DurationString>,

    #[arg(
        long,
        env = "SOS_NO_PROGRESS_BAR",
        action = ArgAction::SetTrue,
        default_value_t = LoadTesterJobArgs::default_no_progress_bar(),
    )]
    #[serde(default = "LoadTesterJobArgs::default_no_progress_bar")]
    pub no_progress_bar: bool,

    #[arg(
        long,
        env = "SOS_THREADS_MAX",
        value_name = "NUM",
        default_value_t = LoadTesterJobArgs::default_threads_max(),
    )]
    #[serde(default = "LoadTesterJobArgs::default_threads_max")]
    pub threads_max: usize,
}

impl Default for LoadTesterJobArgs {
    fn default() -> Self {
        Self {
            duration: None,
            no_progress_bar: Self::default_no_progress_bar(),
            threads_max: Self::default_threads_max(),
        }
    }
}

impl LoadTesterJobArgs {
    const fn default_no_progress_bar() -> bool {
        false
    }

    const fn default_threads_max() -> usize {
        8
    }

    fn print(&self) {
        let Self {
            duration,
            no_progress_bar,
            threads_max,
        } = self;

        info!(
            "duration: {duration}",
            duration = duration
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "None".into(),)
        );
        info!("no_progress_bar: {no_progress_bar}");
        info!("threads_max: {threads_max}");
    }
}

#[derive(Clone, Debug, PartialEq, Parser, Serialize, Deserialize)]
#[clap(rename_all = "kebab-case")]
#[serde(rename_all = "camelCase")]
pub struct RegionArgs {
    #[arg(
        long,
        env = "AWS_ENDPOINT_URL",
        value_name = "URL",
        default_value_t = RegionArgs::default_endpoint(),
    )]
    #[serde(default = "RegionArgs::default_endpoint")]
    pub endpoint: String,

    #[arg(
        long,
        env = "AWS_REGION",
        value_name = "NAME",
        default_value_t = RegionArgs::default_region(),
    )]
    #[serde(default = "RegionArgs::default_region")]
    pub region: String,
}

impl Default for RegionArgs {
    fn default() -> Self {
        Self {
            endpoint: Self::default_endpoint(),
            region: Self::default_region(),
        }
    }
}

impl RegionArgs {
    fn default_endpoint() -> String {
        "s3.amazonaws.com".into()
    }

    fn default_region() -> String {
        "us-east-1".into()
    }

    fn print(&self) {
        let Self { endpoint, region } = self;

        info!("endpoint: {endpoint}");
        info!("region: {region}");
    }
}

impl From<RegionArgs> for Region {
    fn from(value: RegionArgs) -> Self {
        let RegionArgs { endpoint, region } = value;
        Self::Custom { endpoint, region }
    }
}
