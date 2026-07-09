//! `oakterm-ctl` — control and observe a running oakterm daemon over its Unix
//! socket. First slice of the Agent Control API (docs/ideas/32-agent-control-api.md):
//! list panes, send input, read output.

mod client;
mod text;

use std::io;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use oakterm_protocol::message::PaneInfo;

use crate::client::DaemonClient;

#[derive(Parser)]
#[command(
    name = "oakterm-ctl",
    about = "Control and observe a running oakterm daemon"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Operate on panes.
    Pane {
        #[command(subcommand)]
        cmd: PaneCmd,
    },
}

#[derive(Subcommand)]
enum PaneCmd {
    /// List all panes.
    List {
        #[arg(long, value_enum, default_value_t = Format::Table)]
        format: Format,
    },
    /// Send input to a pane (raw bytes written to its PTY).
    Input {
        pane_id: u32,
        text: String,
        /// Append a carriage return (press Enter).
        #[arg(long)]
        enter: bool,
    },
    /// Print a pane's output. Default is the current visible screen.
    Output {
        pane_id: u32,
        /// Read the last N scrollback lines instead of the visible screen.
        #[arg(long)]
        lines: Option<u32>,
    },
}

#[derive(Copy, Clone, ValueEnum)]
enum Format {
    Table,
    Json,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("oakterm-ctl: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> io::Result<()> {
    let Command::Pane { cmd } = cli.command;
    let mut client = DaemonClient::connect()?;
    match cmd {
        PaneCmd::List { format } => {
            let panes = client.list_panes()?;
            match format {
                Format::Table => print_pane_table(&panes),
                Format::Json => print_pane_json(&panes)?,
            }
        }
        PaneCmd::Input {
            pane_id,
            text,
            enter,
        } => {
            let mut bytes = text.into_bytes();
            if enter {
                bytes.push(b'\r');
            }
            client.send_input(pane_id, bytes)?;
        }
        PaneCmd::Output { pane_id, lines } => {
            let out = match lines {
                Some(n) => text::scrollback(&client.scrollback(pane_id, n)?),
                None => text::visible_screen(&client.visible_screen(pane_id)?),
            };
            println!("{out}");
        }
    }
    Ok(())
}

/// Whether a pane's sentinels indicate a live child (Spec-0001: running =
/// `pid > 0` & `exit_code == -1`).
pub(crate) fn pane_running(p: &PaneInfo) -> bool {
    p.pid > 0 && p.exit_code == -1
}

/// Human-readable pane state from the `pid`/`exit_code` sentinels
/// (Spec-0001: exited = pid 0 & exit >= 0; everything else is unknown).
fn pane_state(p: &PaneInfo) -> &'static str {
    if pane_running(p) {
        "running"
    } else if p.pid == 0 && p.exit_code >= 0 {
        "exited"
    } else {
        "unknown"
    }
}

fn print_pane_table(panes: &[PaneInfo]) {
    if panes.is_empty() {
        println!("(no panes)");
        return;
    }
    println!(
        "{:<5} {:<8} {:<8} {:<20} CWD",
        "ID", "SIZE", "STATE", "TITLE"
    );
    for p in panes {
        let size = format!("{}x{}", p.cols, p.rows);
        let title = if p.title.is_empty() { "-" } else { &p.title };
        println!(
            "{:<5} {:<8} {:<8} {:<20} {}",
            p.pane_id,
            size,
            pane_state(p),
            title,
            p.cwd
        );
    }
}

fn print_pane_json(panes: &[PaneInfo]) -> io::Result<()> {
    let arr: Vec<_> = panes
        .iter()
        .map(|p| {
            serde_json::json!({
                "pane_id": p.pane_id,
                "title": p.title,
                "cols": p.cols,
                "rows": p.rows,
                "pid": p.pid,
                "exit_code": p.exit_code,
                "cwd": p.cwd,
                "state": pane_state(p),
            })
        })
        .collect();
    let json = serde_json::to_string_pretty(&arr).map_err(io::Error::other)?;
    println!("{json}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{pane_running, pane_state};
    use oakterm_protocol::message::PaneInfo;

    fn pane(pid: u32, exit_code: i32) -> PaneInfo {
        PaneInfo {
            pane_id: 0,
            title: String::new(),
            cols: 80,
            rows: 24,
            pid,
            exit_code,
            cwd: String::new(),
        }
    }

    #[test]
    fn running_is_pid_present_and_no_exit() {
        assert!(pane_running(&pane(42, -1)));
        assert_eq!(pane_state(&pane(42, -1)), "running");
    }

    #[test]
    fn exited_is_no_pid_with_exit_code() {
        assert!(!pane_running(&pane(0, 0)));
        assert_eq!(pane_state(&pane(0, 0)), "exited");
        assert_eq!(pane_state(&pane(0, 137)), "exited");
    }

    #[test]
    fn unknown_covers_the_sentinel_gaps() {
        // pid 0 & exit -1 = not-yet-spawned; pid set & exit >= 0 = contradictory.
        assert_eq!(pane_state(&pane(0, -1)), "unknown");
        assert_eq!(pane_state(&pane(42, 0)), "unknown");
        assert!(!pane_running(&pane(0, -1)));
    }
}
