use std::{collections::HashSet, path::Path, time::UNIX_EPOCH};

use anyhow::Result;
use chrono::{Local, TimeZone};
use clap::Parser;
use serde::Serialize;

use expensive::{
    app,
    config::{self, Cli, CliCommand, Config},
    db, index,
    sources::{self, SyncMode},
};

fn main() -> Result<()> {
    let cli = Cli::parse();
    let command = cli.command.clone();
    let config = config::load(cli)?;
    match command {
        Some(CliCommand::Doctor { json }) => run_doctor(&config, json),
        Some(CliCommand::Sync { full }) => run_sync(
            &config,
            if full {
                SyncMode::Full
            } else {
                SyncMode::Incremental
            },
        ),
        Some(CliCommand::Report(args)) => expensive::report::run(&config, &args),
        None => app::run(config),
    }
}

fn run_sync(config: &Config, mode: SyncMode) -> Result<()> {
    let summary = sources::sync_configured(config, mode)?;
    for (source, report) in &summary.reports {
        println!(
            "{source}: scanned {}, imported {}, removed {}, skipped {}",
            report.scanned, report.imported, report.removed, report.skipped
        );
    }
    for error in &summary.errors {
        eprintln!("sync error: {error}");
    }
    if summary.generation_before == summary.generation_after {
        println!(
            "usage index unchanged (generation {})",
            summary.generation_before
        );
    } else {
        println!(
            "usage index updated (generation {} -> {})",
            summary.generation_before, summary.generation_after
        );
    }
    if summary.errors.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("{} source(s) failed to sync", summary.errors.len());
    }
}

fn run_doctor(config: &Config, json: bool) -> Result<()> {
    let usage_index = index::UsageIndex::open(&config.index_path)?;
    let index_diagnostics = usage_index.diagnostics()?;
    let freshness = usage_index.freshness()?;

    let opencode_available = config.db_path.is_file();
    let opencode = if opencode_available {
        Some(db::diagnose(&config.db_path)?)
    } else {
        None
    };
    let unknown_tables = if opencode_available {
        db::unknown_usage_tables(&config.db_path)?
    } else {
        Vec::new()
    };

    let mut warnings: Vec<String> = unknown_tables
        .iter()
        .map(|table| {
            format!(
                "opencode database contains usage-like table {table} that this expensive \
                 build does not read; expensive may need updating"
            )
        })
        .collect();
    if let Some(diagnostics) = &opencode {
        warnings.extend(diagnostics.warnings.iter().cloned());
    }
    let mut stale_sources = HashSet::new();
    for source in &freshness {
        let changed = changed_artifacts(source)?;
        if changed > 0 {
            stale_sources.insert(source.id);
            warnings.push(format!(
                "{} source changed since its last sync ({changed} artifact(s)); run: expensive sync",
                source.display_name
            ));
        }
    }

    if json {
        let report = DoctorJson {
            expensive_version: env!("CARGO_PKG_VERSION"),
            index: IndexJson {
                path: index_diagnostics.path.display().to_string(),
                sqlite_version: index_diagnostics.sqlite_version.clone(),
                schema_version: index_diagnostics.schema_version,
                generation: index_diagnostics.generation,
                sources: index_diagnostics.sources,
                artifacts: index_diagnostics.artifacts,
                events: index_diagnostics.events,
            },
            estimate_api_cost: config.estimate_api_cost,
            hidden_providers: config.hidden_providers.iter().cloned().collect(),
            sources: freshness
                .iter()
                .map(|source| SourceJson {
                    kind: source.kind.clone(),
                    display_name: source.display_name.clone(),
                    source_key: source.source_key.clone(),
                    events: source.events,
                    latest_event_ms: source.latest_event_ms,
                    last_sync_ms: source
                        .artifacts
                        .iter()
                        .map(|artifact| artifact.last_scanned_ms)
                        .max(),
                    artifacts: source.artifacts.len(),
                    changed_since_sync: stale_sources.contains(&source.id),
                })
                .collect(),
            opencode: opencode.as_ref().map(|diagnostics| OpencodeJson {
                path: diagnostics.path.clone(),
                sqlite_version: diagnostics.sqlite_version.clone(),
                json_functions: diagnostics.json_functions,
                compatible: diagnostics.is_compatible(),
                assistant_messages: diagnostics.assistant_messages,
                project_scope: diagnostics.project_scope,
                versions: diagnostics.opencode_versions.clone(),
                errors: diagnostics.errors.clone(),
                warnings: diagnostics.warnings.clone(),
                unknown_usage_tables: unknown_tables.clone(),
            }),
            warnings: warnings.clone(),
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
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

        println!("source freshness:");
        for source in &freshness {
            let mut line = format!(
                "  {} ({}): {} event(s)",
                source.display_name, source.source_key, source.events
            );
            if let Some(latest) = source.latest_event_ms {
                line.push_str(&format!(", latest event {}", format_local_ms(latest)));
            }
            if let Some(last_sync) = source
                .artifacts
                .iter()
                .map(|artifact| artifact.last_scanned_ms)
                .max()
            {
                line.push_str(&format!(", last sync {}", format_local_ms(last_sync)));
            }
            println!("{line}");
        }

        if let Some(diagnostics) = &opencode {
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
        }

        for warning in &warnings {
            println!("warning: {warning}");
        }
    }

    if opencode
        .as_ref()
        .is_some_and(|diagnostics| !diagnostics.is_compatible())
    {
        anyhow::bail!(
            "database is incompatible: {}",
            opencode.expect("checked").errors.join("; ")
        );
    }
    Ok(())
}

fn availability(available: bool) -> &'static str {
    if available {
        "available"
    } else {
        "not found"
    }
}

fn format_local_ms(millis: i64) -> String {
    Local
        .timestamp_millis_opt(millis)
        .single()
        .map(|stamp| stamp.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| millis.to_string())
}

fn changed_artifacts(source: &index::SourceFreshness) -> Result<usize> {
    let changed = source
        .artifacts
        .iter()
        .filter(|artifact| match artifact.path.as_deref() {
            Some(path) => changed_since(path, artifact.last_scanned_ms),
            None => false,
        })
        .count();
    let root = Path::new(&source.source_key);
    let roots = match source.kind.as_str() {
        "codex" => vec![root.join("sessions"), root.join("archived_sessions")],
        "pi" => vec![root.to_path_buf()],
        _ => return Ok(changed),
    };
    let known: HashSet<&str> = source
        .artifacts
        .iter()
        .map(|artifact| artifact.key.as_str())
        .collect();
    let added = sources::discover_session_files(&roots)?
        .iter()
        .filter(|path| {
            let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
            !known.contains(key.to_string_lossy().as_ref())
        })
        .count();
    Ok(changed + added)
}

fn changed_since(path: &str, scanned_at_ms: i64) -> bool {
    let primary = Path::new(path);
    if modified_after(primary, scanned_at_ms) {
        return true;
    }
    let wal = format!("{path}-wal");
    let wal_path = Path::new(&wal);
    wal_path.metadata().is_ok_and(|metadata| metadata.len() > 0)
        && modified_after(wal_path, scanned_at_ms)
}

fn modified_after(path: &Path, scanned_at_ms: i64) -> bool {
    path.metadata()
        .and_then(|metadata| metadata.modified())
        .map(|modified| {
            modified
                .duration_since(UNIX_EPOCH)
                .is_ok_and(|since_epoch| since_epoch.as_millis() as i64 > scanned_at_ms)
        })
        .unwrap_or(true)
}

#[derive(Serialize)]
struct DoctorJson {
    expensive_version: &'static str,
    index: IndexJson,
    estimate_api_cost: bool,
    hidden_providers: Vec<String>,
    sources: Vec<SourceJson>,
    opencode: Option<OpencodeJson>,
    warnings: Vec<String>,
}

#[derive(Serialize)]
struct IndexJson {
    path: String,
    sqlite_version: String,
    schema_version: i64,
    generation: i64,
    sources: i64,
    artifacts: i64,
    events: i64,
}

#[derive(Serialize)]
struct SourceJson {
    kind: String,
    display_name: String,
    source_key: String,
    events: i64,
    latest_event_ms: Option<i64>,
    last_sync_ms: Option<i64>,
    artifacts: usize,
    changed_since_sync: bool,
}

#[derive(Serialize)]
struct OpencodeJson {
    path: String,
    sqlite_version: String,
    json_functions: bool,
    compatible: bool,
    assistant_messages: Option<u64>,
    project_scope: bool,
    versions: Vec<String>,
    errors: Vec<String>,
    warnings: Vec<String>,
    unknown_usage_tables: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use expensive::sources::{codex::CodexSource, pi::PiSource, UsageSource};
    use std::{fs, time::Duration};

    fn checks_session_file_changes(kind: &str) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join(kind);
        let (sessions, added_root, source): (_, _, Box<dyn UsageSource>) = match kind {
            "codex" => (
                root.join("sessions"),
                root.join("archived_sessions/2026/01"),
                Box::new(CodexSource::new(root.clone())),
            ),
            "pi" => (
                root.clone(),
                root.join("project"),
                Box::new(PiSource::new(root.clone())),
            ),
            _ => unreachable!(),
        };
        fs::create_dir_all(&sessions).unwrap();
        let original = sessions.join("original.jsonl");
        fs::write(&original, "{}\n").unwrap();
        let mut usage_index = index::UsageIndex::open(&directory.path().join("usage.db")).unwrap();
        source
            .sync(&mut usage_index, SyncMode::Incremental)
            .unwrap();
        let initial = usage_index.freshness().unwrap().remove(0);
        assert_eq!(changed_artifacts(&initial).unwrap(), 0);

        fs::create_dir_all(&added_root).unwrap();
        fs::write(added_root.join("ignored.txt"), "ignored").unwrap();
        assert_eq!(changed_artifacts(&initial).unwrap(), 0);
        fs::write(added_root.join("new.jsonl"), "{}\n").unwrap();
        assert_eq!(changed_artifacts(&initial).unwrap(), 1);

        fs::remove_file(&original).unwrap();
        assert_eq!(changed_artifacts(&initial).unwrap(), 2);
        source
            .sync(&mut usage_index, SyncMode::Incremental)
            .unwrap();
        let updated = usage_index.freshness().unwrap().remove(0);
        assert_eq!(changed_artifacts(&updated).unwrap(), 0);

        let new_file = fs::OpenOptions::new()
            .write(true)
            .open(added_root.join("new.jsonl"))
            .unwrap();
        new_file
            .set_times(fs::FileTimes::new().set_modified(
                UNIX_EPOCH
                    + Duration::from_millis(updated.artifacts[0].last_scanned_ms as u64 + 1000),
            ))
            .unwrap();
        assert_eq!(changed_artifacts(&updated).unwrap(), 1);
    }

    #[test]
    fn detects_added_deleted_and_modified_codex_sessions() {
        checks_session_file_changes("codex");
    }

    #[test]
    fn detects_added_deleted_and_modified_pi_sessions() {
        checks_session_file_changes("pi");
    }
}
