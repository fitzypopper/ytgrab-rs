use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;
use ytgrab::cli::Cli;
use ytgrab::runner;

/// Find the `--config` option in the arguments.
fn find_config(args: &[std::ffi::OsString]) -> Option<std::path::PathBuf> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let text = arg.to_string_lossy();
        if text == "--config" || text == "-C" {
            return iter.next().map(std::path::PathBuf::from);
        }
        if let Some(value) = text.strip_prefix("--config=") {
            return Some(std::path::PathBuf::from(value));
        }
    }
    None
}

fn main() -> ExitCode {
    // A config file is applied first, so the command line wins.
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let config = find_config(&args);
    let args = match &config {
        Some(path) => ytgrab::config::splice(args[0].clone(), args[1..].to_vec(), path),
        None => args,
    };
    let cli = Cli::parse_from(args);

    let level = if cli.verbose {
        "debug"
    } else if cli.quiet {
        "error"
    } else {
        "info"
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(format!("ytgrab={level},warn")))
        .with_writer(std::io::stderr)
        .try_init();

    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: could not start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    match runtime.block_on(runner::run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
