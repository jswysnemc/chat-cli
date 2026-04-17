mod app;
mod cli;
mod config;
mod context;
mod error;
mod media;
mod output;
mod provider;
mod render;
mod session;
mod tool;

use clap::Parser;

#[tokio::main]
async fn main() {
    let cli = cli::Cli::parse();
    if let Err(err) = app::run(cli).await {
        eprintln!("{}", err.message);
        std::process::exit(err.code);
    }
}
