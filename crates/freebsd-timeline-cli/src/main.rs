use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use clap::{Parser, Subcommand};
use serde::de::DeserializeOwned;
use serde::Serialize;

use freebsd_timeline_core::client::Client;
use freebsd_timeline_core::model::{ActionResult, BootEnv, Dataset, DiffEntry, PoolHealth, Snapshot, Status};
use freebsd_timeline_core::protocol::Request;
use freebsd_timeline_core::retention::PrunePlan;
use freebsd_timeline_core::time::{self, relative};
use freebsd_timeline_core::util::human_size;

#[derive(Parser)]
#[command(name = "freebsd-timeline", version, about = "ZFS snapshots, boot environments and backups")]
struct Cli {
    /// Path to the freebsd-timelined socket (default: $FREEBSD_TIMELINE_SOCKET or /var/run/freebsd-timelined.sock)
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    /// Print raw JSON
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Overview: pools, snapshot counts, last snapshot
    Status,
    /// List snapshots (only freebsd-timeline's unless --all)
    #[command(alias = "ls")]
    List {
        dataset: Option<String>,
        /// Include snapshots made by other tools
        #[arg(short, long)]
        all: bool,
    },
    /// Take a snapshot now
    Snap {
        dataset: String,
        /// Label in the name, e.g. before-rice-v4 (default: manual)
        #[arg(short, long)]
        label: Option<String>,
        /// Include child datasets
        #[arg(short, long)]
        recursive: bool,
    },
    /// Destroy a snapshot
    Destroy {
        snapshot: String,
        #[arg(short, long)]
        recursive: bool,
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Roll a dataset back to a snapshot
    Rollback {
        snapshot: String,
        /// Also destroy snapshots newer than the target (required by ZFS if any exist)
        #[arg(long)]
        destroy_newer: bool,
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Show files changed since a snapshot (or between two)
    Diff { from: String, to: Option<String> },
    /// Boot environments
    #[command(subcommand)]
    Be(BeCmd),
    /// Pool health, scrub status and errors
    Health,
    /// Datasets, mountpoints and encryption state
    Datasets,
    /// Apply the retention policy
    Prune {
        /// Only show what would be destroyed
        #[arg(short = 'n', long)]
        dry_run: bool,
    },
    /// Send snapshots to the configured backup host
    Backup { dataset: Option<String> },
    /// Panic lock: unmount and lock every encrypted dataset not holding up the system
    Lock,
    /// Load a dataset's key and mount it again
    Unlock { dataset: String },
    /// Create a boot environment, then run a command (e.g. an upgrade)
    Guard {
        #[arg(short, long, default_value = "pre-upgrade")]
        label: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
}

#[derive(Subcommand)]
enum BeCmd {
    /// List boot environments
    #[command(alias = "ls")]
    List,
    /// Create a boot environment from the running system
    Create { name: Option<String> },
    /// Boot into a boot environment next time
    Activate {
        name: String,
        /// Only for the next boot; falls back afterwards
        #[arg(short, long)]
        temporary: bool,
    },
    /// Destroy a boot environment
    Destroy {
        name: String,
        #[arg(short = 'y', long)]
        yes: bool,
    },
}

type CliResult = Result<(), Box<dyn std::error::Error>>;

struct App {
    client: Client,
    json: bool,
}

impl App {
    fn call<T: DeserializeOwned + Serialize>(&self, req: Request) -> Result<T, Box<dyn std::error::Error>> {
        Ok(self.client.call::<T>(&req)?)
    }

    /// Returns true if the caller should print a human view.
    fn json_out(&self, v: &impl Serialize) -> bool {
        if self.json {
            println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
        }
        !self.json
    }
}

fn confirm(question: &str, yes: bool) -> Result<(), Box<dyn std::error::Error>> {
    if yes {
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        return Err("refusing without --yes when not on a terminal".into());
    }
    print!("{question} [y/N] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    if matches!(answer.trim(), "y" | "Y" | "yes") {
        Ok(())
    } else {
        Err("cancelled".into())
    }
}

fn print_results(results: &[ActionResult]) -> bool {
    let mut all_ok = true;
    for r in results {
        all_ok &= r.ok;
        println!("{} {}: {}", if r.ok { "ok  " } else { "FAIL" }, r.target, r.message);
    }
    if results.is_empty() {
        println!("nothing to do");
    }
    all_ok
}

fn print_snapshots(snaps: &[Snapshot]) {
    let now = time::now();
    println!("{:<12} {:>8}  SNAPSHOT", "CREATED", "USED");
    for s in snaps {
        println!("{:<12} {:>8}  {}", relative(s.created, now), human_size(s.used), s.full_name());
    }
}

fn run(cli: Cli) -> CliResult {
    let app = App {
        client: cli.socket.map(Client::new).unwrap_or_default(),
        json: cli.json,
    };

    match cli.cmd {
        Cmd::Status => {
            let st: Status = app.call(Request::Status)?;
            if app.json_out(&st) {
                println!("freebsd-timeline {} (daemon backend: {})", st.version, st.backend);
                let last = st.last_snapshot.map_or("never".into(), |t| relative(t, time::now()));
                println!("snapshots  {} total, {} by freebsd-timeline, last {}", st.snapshots, st.managed, last);
                println!(
                    "watching   {}",
                    if st.watched.is_empty() { "nothing (see freebsd-timeline.toml)".into() } else { st.watched.join(", ") }
                );
                println!("backup     {}", st.backup_target.as_deref().unwrap_or("not configured"));
                println!("boot envs  {}", if st.boot_envs_supported { "supported" } else { "unsupported (no bectl)" });
                for p in &st.pools {
                    println!("pool       {} {} ({} of {} used)", p.name, p.health, human_size(p.alloc), human_size(p.size));
                }
            }
        }
        Cmd::List { dataset, all } => {
            let mut snaps: Vec<Snapshot> = app.call(Request::ListSnapshots { dataset })?;
            if !all {
                snaps.retain(|s| s.is_managed());
            }
            if app.json_out(&snaps) {
                print_snapshots(&snaps);
            }
        }
        Cmd::Snap { dataset, label, recursive } => {
            let name: String = app.call(Request::CreateSnapshot { dataset, label, recursive })?;
            if app.json_out(&name) {
                println!("created {name}");
            }
        }
        Cmd::Destroy { snapshot, recursive, yes } => {
            confirm(&format!("Destroy {snapshot}{}?", if recursive { " and its children" } else { "" }), yes)?;
            let name: String = app.call(Request::DestroySnapshot { snapshot, recursive })?;
            if app.json_out(&name) {
                println!("destroyed {name}");
            }
        }
        Cmd::Rollback { snapshot, destroy_newer, yes } => {
            let warn = if destroy_newer { " Newer snapshots will be destroyed." } else { "" };
            confirm(&format!("Roll back to {snapshot}? Changes since then are lost.{warn}"), yes)?;
            let name: String = app.call(Request::Rollback { snapshot, destroy_newer })?;
            if app.json_out(&name) {
                println!("rolled back to {name}");
            }
        }
        Cmd::Diff { from, to } => {
            let entries: Vec<DiffEntry> = app.call(Request::Diff { from, to })?;
            if app.json_out(&entries) {
                for e in &entries {
                    match &e.new_path {
                        Some(new) => println!("{}  {} -> {}", e.change.symbol(), e.path, new),
                        None => println!("{}  {}", e.change.symbol(), e.path),
                    }
                }
            }
        }
        Cmd::Be(be) => match be {
            BeCmd::List => {
                let bes: Vec<BootEnv> = app.call(Request::ListBootEnvs)?;
                if app.json_out(&bes) {
                    println!("{:<28} {:<7} {:>8}  CREATED", "NAME", "ACTIVE", "SPACE");
                    for b in &bes {
                        let flags = format!(
                            "{}{}",
                            if b.active_now { "N" } else { "" },
                            if b.active_on_reboot { "R" } else { "" }
                        );
                        println!("{:<28} {:<7} {:>8}  {}", b.name, if flags.is_empty() { "-" } else { &flags }, b.space, b.created);
                    }
                }
            }
            BeCmd::Create { name } => {
                let name: String = app.call(Request::CreateBootEnv { name, label: None })?;
                if app.json_out(&name) {
                    println!("created boot environment {name}");
                }
            }
            BeCmd::Activate { name, temporary } => {
                let name: String = app.call(Request::ActivateBootEnv { name, temporary })?;
                if app.json_out(&name) {
                    println!("{name} will be used on {} boot", if temporary { "the next" } else { "every" });
                }
            }
            BeCmd::Destroy { name, yes } => {
                confirm(&format!("Destroy boot environment {name}?"), yes)?;
                let name: String = app.call(Request::DestroyBootEnv { name })?;
                if app.json_out(&name) {
                    println!("destroyed {name}");
                }
            }
        },
        Cmd::Health => {
            let pools: Vec<PoolHealth> = app.call(Request::PoolHealth)?;
            if app.json_out(&pools) {
                for p in &pools {
                    println!("{}  {}  {} used, {} free", p.name, p.health, human_size(p.alloc), human_size(p.free));
                    if let Some(scan) = &p.scan {
                        println!("  scan:   {scan}");
                    }
                    if let Some(errors) = &p.errors {
                        println!("  errors: {errors}");
                    }
                }
            }
        }
        Cmd::Datasets => {
            let ds: Vec<Dataset> = app.call(Request::ListDatasets)?;
            if app.json_out(&ds) {
                println!("{:<28} {:>8} {:>8}  {:<10} MOUNTPOINT", "NAME", "USED", "AVAIL", "KEY");
                for d in &ds {
                    let key = match (d.encrypted, d.key_loaded) {
                        (false, _) => "-",
                        (true, true) => "unlocked",
                        (true, false) => "locked",
                    };
                    println!(
                        "{:<28} {:>8} {:>8}  {:<10} {}",
                        d.name,
                        human_size(d.used),
                        human_size(d.avail),
                        key,
                        d.mountpoint.as_deref().unwrap_or("-")
                    );
                }
            }
        }
        Cmd::Prune { dry_run } => {
            if dry_run {
                let plan: PrunePlan = app.call(Request::PrunePlan)?;
                if app.json_out(&plan) {
                    println!("would destroy {} snapshot(s), keep {}", plan.destroy.len(), plan.keep.len());
                    print_snapshots(&plan.destroy);
                }
            } else {
                let results: Vec<ActionResult> = app.call(Request::Prune)?;
                if app.json_out(&results) && !print_results(&results) {
                    return Err("some snapshots could not be destroyed".into());
                }
            }
        }
        Cmd::Backup { dataset } => {
            let results: Vec<ActionResult> = app.call(Request::Backup { dataset })?;
            if app.json_out(&results) && !print_results(&results) {
                return Err("backup failed".into());
            }
        }
        Cmd::Lock => {
            let results: Vec<ActionResult> = app.call(Request::LockAll)?;
            if app.json_out(&results) && !print_results(&results) {
                return Err("some datasets could not be locked".into());
            }
        }
        Cmd::Unlock { dataset } => {
            let passphrase = rpassword::prompt_password(format!("Passphrase for {dataset}: "))?;
            let name: String = app.call(Request::Unlock { dataset, passphrase })?;
            if app.json_out(&name) {
                println!("unlocked {name}");
            }
        }
        Cmd::Guard { label, command } => {
            let be: String = app
                .call(Request::CreateBootEnv { name: None, label: Some(label) })
                .map_err(|e| format!("could not create a boot environment, not running the command: {e}"))?;
            eprintln!("freebsd-timeline: created boot environment {be}");
            let status = Command::new(&command[0]).args(&command[1..]).status()?;
            if !status.success() {
                eprintln!("freebsd-timeline: command failed. To go back to how things were:");
                eprintln!("  freebsd-timeline be activate {be} && shutdown -r now");
                return Err(format!("{} exited with {status}", command[0]).into());
            }
        }
    }
    Ok(())
}

/// One-line advice for failures people actually run into.
fn hint(message: &str) -> Option<&'static str> {
    if message.contains("more recent snapshots") {
        Some("add --destroy-newer to destroy the newer snapshots too")
    } else if message.contains("Host key verification failed") {
        Some("run `doas ssh <backup target>` once as root to accept its host key")
    } else if message.contains("Permission denied (publickey") {
        Some("root's SSH key must be in the backup user's authorized_keys")
    } else if message.contains("permission denied") && message.contains("recv") {
        Some("on the backup host run: zfs allow -u <user> receive,create,mount,mountpoint <pool>")
    } else {
        None
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let message = e.to_string();
            eprintln!("freebsd-timeline: {message}");
            if let Some(h) = hint(&message) {
                eprintln!("hint: {h}");
            }
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::hint;

    #[test]
    fn hints_match_real_zfs_and_ssh_errors() {
        let rollback = "cannot rollback to 'a@b': more recent snapshots or bookmarks exist";
        assert!(hint(rollback).unwrap().contains("--destroy-newer"));
        assert!(hint("Host key verification failed.").unwrap().contains("host key"));
        assert!(hint("something unrelated").is_none());
    }
}
