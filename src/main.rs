use clap::Parser;

mod cli;
mod config;
mod git;
mod model;
mod operations;

fn main() {
    if let Err(error) = cli::run(cli::Cli::parse()) {
        eprintln!("wt: {error}");
        std::process::exit(1);
    }
}
