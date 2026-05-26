//! Main entry point for ModelWire server.

use clap::Parser;
use modelwire_core as core;
use modelwire_db::Database;
use modelwire_server::ServerState;
use std::sync::Arc;
use std::time::Duration;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// CLI arguments.
#[derive(Parser, Debug)]
#[command(name = "modelwire")]
#[command(about = "A Codex-first Responses API relay")]
struct Args {
    /// Path to configuration file.
    #[arg(short, long, default_value = "modelwire.toml")]
    config: String,

    /// Subcommand.
    #[command(subcommand)]
    command: Option<Commands>,
}

/// Subcommands.
#[derive(clap::Subcommand, Debug)]
enum Commands {
    /// Serve the API relay.
    Serve,
    /// Export configuration.
    ExportConfig,
    /// Run database migrations.
    Migrate,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer().with_target(true))
        .init();

    let args = Args::parse();

    // Load configuration
    let config = core::Config::from_file(&args.config)
        .map_err(|e| format!("Failed to load config: {}", e))?;

    tracing::info!("Loaded config from {}", args.config);

    // Initialize database
    let db = Database::connect(&config.server.database_url).await?;

    // Run migrations
    db.run_migrations().await?;

    // Create server state
    let state = Arc::new(ServerState {
        config,
        db,
        probe_cache: dashmap::DashMap::new(),
        probe_locks: dashmap::DashMap::new(),
        key_limiter_counters: dashmap::DashMap::new(),
        ip_limiter_counters: dashmap::DashMap::new(),
        archive_writers: tokio::sync::Mutex::new(std::collections::HashMap::new()),
    });

    modelwire_server::runtime_config::ensure_operational_config_seeded(state.as_ref())
        .await
        .map_err(|e| format!("Failed to seed runtime config: {}", e.message))?;

    // Spawn janitor task if running serve command
    if matches!(args.command.as_ref(), Some(Commands::Serve) | None) {
        let cleanup_interval = Duration::from_secs(300); // 5 minutes
        let db_for_janitor = state.db.clone();

        // Spawn a background task that periodically runs cleanup
        tokio::spawn(async move {
            use modelwire_server::Janitor;
            let mut cleanup_timer = tokio::time::interval(cleanup_interval);
            cleanup_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            // Run cleanup once at startup
            let janitor = Janitor::new(db_for_janitor.clone());
            match janitor.run_cleanup().await {
                Ok(report) => {
                    tracing::info!(
                        responses = report.responses_deleted,
                        items = report.response_items_deleted,
                        handles = report.handles_deleted,
                        probes = report.probes_deleted,
                        logs = report.logs_deleted,
                        vacuum = report.vacuum_performed,
                        "Initial janitor cleanup completed"
                    );
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Initial janitor cleanup failed");
                }
            }

            // Then run periodically
            loop {
                cleanup_timer.tick().await;
                let janitor = Janitor::new(db_for_janitor.clone());
                match janitor.run_cleanup().await {
                    Ok(report) => {
                        tracing::info!(
                            responses = report.responses_deleted,
                            items = report.response_items_deleted,
                            handles = report.handles_deleted,
                            probes = report.probes_deleted,
                            logs = report.logs_deleted,
                            vacuum = report.vacuum_performed,
                            "Periodic janitor cleanup completed"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Periodic janitor cleanup failed");
                    }
                }
            }
        });

        tracing::info!(
            "Janitor task started with {} interval",
            cleanup_interval.as_secs()
        );
    }

    match args.command.as_ref() {
        Some(Commands::Serve) | None => {
            // Start server
            tracing::info!("Starting ModelWire server...");
            modelwire_server::server::serve(state).await?;
        }
        Some(Commands::ExportConfig) => {
            // Export config (redacted)
            let json = serde_json::to_string_pretty(&state.config.to_redacted_json())?;
            println!("{}", json);
        }
        Some(Commands::Migrate) => {
            // Migrations already run above, but can trigger explicitly
            tracing::info!("Running migrations...");
            state.db.run_migrations().await?;
            tracing::info!("Migrations complete.");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use std::path::PathBuf;

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crate should live inside workspace")
            .to_path_buf()
    }

    #[test]
    fn cli_accepts_config_before_serve_subcommand() {
        let args = Args::try_parse_from(["modelwire", "--config", "modelwire.toml", "serve"])
            .expect("CLI should accept config before serve subcommand");
        assert_eq!(args.config, "modelwire.toml");
        assert!(matches!(args.command, Some(Commands::Serve)));
    }

    #[test]
    fn cli_defaults_to_modelwire_toml() {
        let args = Args::try_parse_from(["modelwire", "serve"])
            .expect("CLI should parse serve with default config");
        assert_eq!(args.config, "modelwire.toml");
        assert!(matches!(args.command, Some(Commands::Serve)));
    }

    #[test]
    fn cli_help_flag_parses_as_help_display() {
        let err = Args::try_parse_from(["modelwire", "--help"])
            .expect_err("help flag should short-circuit parse as display help");
        assert_eq!(err.kind(), ErrorKind::DisplayHelp);
    }

    #[test]
    fn config_loading_invalid_path_fails_with_clear_error_prefix() {
        let missing_path = workspace_root()
            .join("modelwire-server")
            .join("tests")
            .join("fixtures")
            .join("missing-does-not-exist.toml");
        let err = core::Config::from_file(&missing_path)
            .expect_err("missing config path should fail fast");
        let wrapped = format!("Failed to load config: {}", err);
        assert!(
            wrapped.starts_with("Failed to load config:"),
            "error message should keep clear config-loading prefix"
        );
    }
}
