//! `telemetry-controller`'s command-line surface.
//!
//! `envoy telemetry ...` resolves and executes this binary through envoy's
//! existing bundle/command dispatch (see `.envoy/commands.json`), so this
//! crate's own argument parsing is independent of envoy-cli's.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "telemetry-controller",
    about = "Lifecycle controller for the envoy telemetry bundle's shared studio infrastructure",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Start Collector/Tempo/Grafana/the ingestion sweep. Idempotent.
    Start {
        /// Which runtime to use: `auto` (default), `compose`, or `native`
        /// (Linux x86-64 only).
        #[arg(long, default_value = "auto")]
        runtime: String,
    },
    /// Stop the shared infrastructure. Always preserves telemetry data.
    Stop,
    /// Report whether the server is running, its runtime, retention, and
    /// sanitized configuration (no headers/credentials beyond the
    /// generated Grafana viewer password).
    Status,
    /// Show logs for one service, or all services if omitted.
    Logs {
        #[arg(value_parser = ["collector", "tempo", "grafana", "sweep"])]
        service: Option<String>,
    },
    /// Open the Grafana dashboard in the default browser.
    Open,
    /// Update the configured Tempo retention period.
    Configure {
        #[arg(long)]
        retention_days: u32,
    },
    /// Run the file-drop ingestion sweep. Used as the sweep service's own
    /// container/process entrypoint; also invokable directly for a single
    /// pass (omit `--daemon`).
    Sweep {
        #[arg(long)]
        drop_dir: PathBuf,
        #[arg(long)]
        collector_endpoint: String,
        /// Shared bearer token to present to the Collector's OTLP
        /// receiver. Explicit (via this flag or the `COLLECTOR_BEARER_TOKEN`
        /// env var) rather than read from `telemetry-controller`'s own
        /// state file, since the sweep normally runs inside its own
        /// container with no access to the host's config root -- falling
        /// back to `load_state()` there would always resolve to "no
        /// token configured" and cause every forward attempt to be
        /// rejected as unauthenticated. Confirmed directly: the sweep
        /// silently retried every dropped file forever until this was
        /// wired through explicitly.
        #[arg(long, env = "COLLECTOR_BEARER_TOKEN")]
        bearer_token: Option<String>,
        /// Run continuously, polling every `--poll-interval-seconds`,
        /// instead of a single pass.
        #[arg(long)]
        daemon: bool,
        #[arg(long, default_value_t = 10)]
        poll_interval_seconds: u64,
    },
}

pub fn run(argv: &[String]) -> i32 {
    let cli =
        match Cli::try_parse_from(std::iter::once(&"telemetry-controller".to_string()).chain(argv))
        {
            Ok(cli) => cli,
            Err(error) => {
                let _ = error.print();
                return error.exit_code();
            }
        };

    match cli.command {
        Commands::Start { runtime } => crate::commands::start(&runtime),
        Commands::Stop => crate::commands::stop(),
        Commands::Status => crate::commands::status(),
        Commands::Logs { service } => crate::commands::logs(service.as_deref()),
        Commands::Open => crate::commands::open(),
        Commands::Configure { retention_days } => crate::commands::configure(retention_days),
        Commands::Sweep {
            drop_dir,
            collector_endpoint,
            bearer_token,
            daemon,
            poll_interval_seconds,
        } => crate::commands::sweep_command(
            &drop_dir,
            &collector_endpoint,
            bearer_token.as_deref(),
            daemon,
            Duration::from_secs(poll_interval_seconds),
        ),
    }
}
