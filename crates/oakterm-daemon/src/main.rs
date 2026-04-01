use oakterm_daemon::server;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

fn main() {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("{}", version_string());
        return;
    }

    if std::env::args().any(|a| a == "--init-config") {
        run_init_config();
        return;
    }

    let verbose = std::env::args().filter(|a| a == "-v").count()
        + std::env::args().filter(|a| a == "--verbose").count();

    init_tracing(verbose);

    let persist = std::env::args().any(|a| a == "--persist");

    let config = oakterm_config::load_config();
    if let Some(err) = &config.error {
        warn!("config error (using defaults): {err}");
    }

    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    rt.block_on(async {
        let mut daemon = server::Daemon::new(80, 24).expect("failed to create daemon");
        daemon.set_persist(persist);

        if config.config.scrollback_archive {
            daemon.set_archive_config(server::ArchiveConfig {
                max_bytes: config.config.scrollback_archive_limit,
            });
        }

        info!(
            path = %daemon.socket_path().display(),
            persist,
            archive = config.config.scrollback_archive,
            "daemon listening",
        );
        if let Err(e) = daemon.run().await {
            error!(error = %e, "daemon exited with error");
        }
    });
}

fn run_init_config() {
    let config_dir = oakterm_config::config_dir();
    match oakterm_config::init_config(&config_dir) {
        Ok(result) => {
            println!("Config directory: {}", result.config_dir.display());
            if result.created_config {
                println!("  Created config.lua");
            } else {
                println!("  config.lua already exists (unchanged)");
            }
            if result.created_luarc {
                println!("  Created .luarc.json");
            } else {
                println!("  .luarc.json already exists (unchanged)");
            }
            if result.updated_stubs {
                println!("  Updated types/oakterm.lua");
            } else {
                println!("  types/oakterm.lua is up to date");
            }
        }
        Err(e) => {
            eprintln!("error: failed to initialize config: {e}");
            std::process::exit(1);
        }
    }
}

fn init_tracing(verbose: usize) {
    let filter = match verbose {
        0 => EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        1 => EnvFilter::new("debug"),
        _ => EnvFilter::new("trace"),
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_file(false)
        .with_line_number(false)
        .init();
}

fn version_string() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let channel = env!("RELEASE_CHANNEL");
    let source = env!("INSTALL_SOURCE");
    let sha = option_env!("VERGEN_GIT_SHA").unwrap_or("unknown");
    let short_sha = &sha[..sha.len().min(7)];

    match channel {
        "dev" => format!("oakterm-daemon {version}-dev+{short_sha} ({channel}, {source})"),
        _ => format!("oakterm-daemon {version} ({channel}, {source})"),
    }
}
