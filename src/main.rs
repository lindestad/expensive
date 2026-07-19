use anyhow::Result;
use clap::Parser;

use expensive::{
    app,
    config::{self, Cli, CliCommand},
    db,
};

fn main() -> Result<()> {
    let cli = Cli::parse();
    let command = cli.command;
    let config = config::load(cli)?;
    match command {
        Some(CliCommand::Doctor) => run_doctor(&config.db_path),
        None => app::run(config),
    }
}

fn run_doctor(path: &std::path::Path) -> Result<()> {
    let diagnostics = db::diagnose(path)?;
    println!("database: {}", diagnostics.path);
    println!("sqlite: {}", diagnostics.sqlite_version);
    println!(
        "json functions: {}",
        if diagnostics.json_functions {
            "available"
        } else {
            "missing"
        }
    );
    println!(
        "usage schema: {}",
        if diagnostics.is_compatible() {
            "compatible"
        } else {
            "incompatible"
        }
    );
    if let Some(messages) = diagnostics.assistant_messages {
        println!("assistant messages: {messages}");
    }
    println!(
        "project scope: {}",
        if diagnostics.project_scope {
            "available"
        } else {
            "unavailable"
        }
    );
    if !diagnostics.opencode_versions.is_empty() {
        println!(
            "opencode versions: {}",
            diagnostics.opencode_versions.join(", ")
        );
    }
    for warning in &diagnostics.warnings {
        println!("warning: {warning}");
    }
    if diagnostics.is_compatible() {
        Ok(())
    } else {
        anyhow::bail!(
            "database is incompatible: {}",
            diagnostics.errors.join("; ")
        )
    }
}
