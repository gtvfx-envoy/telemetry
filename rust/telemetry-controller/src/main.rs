//! `telemetry-controller` -- lifecycle controller for the envoy telemetry
//! bundle's shared studio infrastructure (Collector, Tempo, Grafana, and
//! the file-drop ingestion sweep).
//!
//! Fully independent of `envoy`/`envoy-cli` (its own Cargo workspace, own
//! release pipeline), mirroring how `engit` is independent from
//! `envoy-cli` today. Registered as the telemetry bundle's own managed
//! command via `.envoy/commands.json`, so `envoy telemetry ...` resolves
//! and executes this binary through envoy's existing bundle/command
//! dispatch -- no telemetry-specific dependencies are added to `envoy-cli`
//! itself.

mod cli;
mod commands;
mod compose;
mod config_root;
mod grafana;
mod native;
mod ports;
mod process;
mod runtime;
mod state;
mod sweep;
mod tempo_config;

fn main() {
    let argv = std::env::args().skip(1).collect::<Vec<_>>();
    std::process::exit(cli::run(&argv));
}
