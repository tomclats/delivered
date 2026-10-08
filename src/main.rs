use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use delivered::{measure, render_html, render_json, render_table, Error};

#[derive(Parser)]
#[command(
    name = "delivered",
    version,
    about = "Turn a folder of stems into a client delivery sheet."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print a delivery table.
    Scan {
        /// Session directory, or one WAV or AIFF file.
        path: PathBuf,
        /// Exit 2 when a stem clips, drifts, or starts late.
        #[arg(long)]
        strict: bool,
    },
    /// Write an HTML delivery sheet.
    Sheet {
        /// Session directory, or one WAV or AIFF file.
        path: PathBuf,
        /// Destination HTML path.
        #[arg(short, long)]
        output: PathBuf,
        /// Exit 2 when a stem clips, drifts, or starts late.
        #[arg(long)]
        strict: bool,
    },
    /// Write a canonical JSON receipt. Prints it when -o is omitted.
    Receipt {
        /// Session directory, or one WAV or AIFF file.
        path: PathBuf,
        /// Destination JSON path. Omit to write the receipt to stdout.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Exit 2 when a stem clips, drifts, or starts late.
        #[arg(long)]
        strict: bool,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("delivered: {err}");
            ExitCode::from(1u8)
        }
    }
}

fn run() -> Result<ExitCode, Error> {
    let cli = Cli::parse();
    match cli.command {
        Command::Scan { path, strict } => {
            let delivery = measure(&path)?;
            print!("{}", render_table(&delivery));
            Ok(finish(strict, delivery.session.notes.is_empty()))
        }
        Command::Sheet {
            path,
            output,
            strict,
        } => {
            let delivery = measure(&path)?;
            let html = render_html(&delivery);
            fs::write(&output, html).map_err(|source| Error::io(&output, source))?;
            println!("wrote {}", output.display());
            let count = delivery.session.notes.len();
            if count > 0 {
                let word = if count == 1 { "note" } else { "notes" };
                println!("{count} {word}");
            }
            Ok(finish(strict, count == 0))
        }
        Command::Receipt {
            path,
            output,
            strict,
        } => {
            let delivery = measure(&path)?;
            let json = render_json(&delivery);
            if let Some(output) = output {
                fs::write(&output, json).map_err(|source| Error::io(&output, source))?;
                println!("wrote {}", output.display());
            } else {
                print!("{json}");
            }
            Ok(finish(strict, delivery.session.notes.is_empty()))
        }
    }
}

fn finish(strict: bool, clean: bool) -> ExitCode {
    if strict && !clean {
        ExitCode::from(2u8)
    } else {
        ExitCode::SUCCESS
    }
}
