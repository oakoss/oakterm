use oakterm_daemon::server;

fn main() {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("{}", version_string());
        return;
    }

    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    rt.block_on(async {
        let daemon = server::Daemon::new(80, 24).expect("failed to create daemon");
        eprintln!(
            "oakterm-daemon listening on {}",
            daemon.socket_path().display()
        );
        if let Err(e) = daemon.run().await {
            eprintln!("daemon error: {e}");
        }
    });
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
