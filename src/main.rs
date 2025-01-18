#[macro_use]
extern crate log;
extern crate log_once;

use crate::args::{Args, Mode};
use clap::Parser;
use client::run_client;
use server::run_server;

mod args;
mod base64;
mod client;
mod interfaces;
mod server;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    std::env::set_var(
        "RUST_LOG",
        "debug,actix=off,reqwest=off,hyper=off,h2=off,tracing=off",
    );

    let args = Args::parse();
    if args.verbose {
        std::env::set_var("RUST_LOG", "trace,h2=debug,tracing=off"); // more logs!!
    }

    env_logger::init();

    match args.mode {
        Mode::Server => run_server(&args).await,
        Mode::Client => run_client(&args).await,
    }

    Ok(())
}
