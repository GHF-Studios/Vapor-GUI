#![cfg_attr(windows, windows_subsystem = "windows")]

use clap::{Parser, Subcommand};
use vapor_gui_core::GuiMode;

#[derive(Debug, Parser)]
#[command(name = "vapor-setup", about = "Install standalone Vapor")]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Install and activate the latest signed Vapor release for this platform.
    Install {
        #[arg(long, default_value = "stable")]
        channel: String,
        /// Print only the canonical installed release root for automation.
        #[arg(long)]
        print_root: bool,
    },
}

fn main() -> eframe::Result<()> {
    match Args::parse().command {
        Some(Command::Install { channel, print_root }) => {
            let report = vapor_installer::standalone::install_latest(&channel)
                .unwrap_or_else(|error| {
                    eprintln!("Vapor installation failed: {error}");
                    std::process::exit(1);
                });
            if print_root {
                println!("{}", report.release_root.display());
            } else {
                println!(
                    "Vapor {} is active from {} ({} / {}).{}",
                    report.version,
                    report.release_root.display(),
                    report.channel,
                    report.platform,
                    if report.already_installed {
                        " The verified version was already installed."
                    } else {
                        ""
                    }
                );
            }
            Ok(())
        }
        None => vapor_gui::run(GuiMode::Setup),
    }
}
