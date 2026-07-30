use clap::Parser;

mod app;
mod background;
mod cli;
mod config;
mod git;
mod github;
mod model;
mod operations;
mod terminal;
mod tui;
mod ui;

fn main() {
    match cli::run(cli::Cli::parse()) {
        Ok(selection) => {
            if let Err(error) =
                terminal::write_selection(std::io::stdout().lock(), selection.as_deref())
            {
                eprintln!("wt: failed to write selection: {error}");
                std::process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("wt: {error}");
            std::process::exit(1);
        }
    }
}
