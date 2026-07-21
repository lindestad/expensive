use anyhow::Result;
use clap::Parser;

use expensive::{
    app,
    config::{self, Cli, CliCommand, Config},
    db, index,
};

fn main() -> Result<()> {
    let cli = Cli::parse();
    let command = cli.command.clone();
    let config = config::load(cli)?;
    match command {
        Some(CliCommand::Doctor) => run_doctor(&config),
        Some(CliCommand::Report(args)) => expensive::report::run(&config, &args),
        None => app::run(config),
    }
}

fn run_doctor(config: &Config) -> Result<()> {
    let index = index::UsageIndex::open(&config.index_path)?;
    let index_diagnostics = index.diagnostics()?;
    println!("usage index: {}", index_diagnostics.path.display());
    println!("index sqlite: {}", index_diagnostics.sqlite_version);
    println!("index schema: {}", index_diagnostics.schema_version);
    println!("index generation: {}", index_diagnostics.generation);
    println!("indexed sources: {}", index_diagnostics.sources);
    println!("indexed artifacts: {}", index_diagnostics.artifacts);
    println!("indexed events: {}", index_diagnostics.events);
    println!(
        "hidden providers: {}",
        if config.hidden_providers.is_empty() {
            "none".to_string()
        } else {
            config
                .hidden_providers
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    println!(
        "API cost estimates: {}",
        if config.estimate_api_cost {
            "enabled"
        } else {
            "disabled"
        }
    );

    let opencode_available = config.db_path.is_file();
    println!(
        "opencode source: {} ({})",
        config.db_path.display(),
        availability(opencode_available)
    );
    println!(
        "copilot source: {} ({})",
        config.copilot_home.display(),
        availability(config.copilot_home.join("session-store.db").is_file())
    );
    println!(
        "codex source: {} ({})",
        config.codex_home.display(),
        availability(
            config.codex_home.join("sessions").is_dir()
                || config.codex_home.join("archived_sessions").is_dir()
        )
    );
    println!(
        "pi source: {} ({})",
        config.pi_sessions_root.display(),
        availability(config.pi_sessions_root.is_dir())
    );

    if !opencode_available {
        return Ok(());
    }

    let diagnostics = db::diagnose(&config.db_path)?;
    println!("opencode sqlite: {}", diagnostics.sqlite_version);
    println!(
        "opencode json functions: {}",
        if diagnostics.json_functions {
            "available"
        } else {
            "missing"
        }
    );
    println!(
        "opencode usage schema: {}",
        if diagnostics.is_compatible() {
            "compatible"
        } else {
            "incompatible"
        }
    );
    if let Some(messages) = diagnostics.assistant_messages {
        println!("opencode assistant messages: {messages}");
    }
    println!(
        "opencode project scope: {}",
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

fn availability(available: bool) -> &'static str {
    if available {
        "available"
    } else {
        "not found"
    }
}
