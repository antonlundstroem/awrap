//! aw: find an Argo CD app and act on it. See README.md.

mod apps;
mod pick;
mod view;

use std::env;
use std::io::{self, BufRead, IsTerminal, Write};
use std::os::unix::process::CommandExt;
use std::process::{self, Command, Stdio};
use std::slice;
use std::time::{SystemTime, UNIX_EPOCH};

use apps::{App, Workload};

const USAGE: &str = "\
aw: find an Argo CD app and act on it

usage:
  aw [query]                         pick an app, print its name
  aw <command> [query] [-- flags]    pick an app, run the command on it

commands:
  g  get        d  diff       s  sync*      l  logs       h  history
  m  manifests  w  wait       r  refresh    rs restart*   dp deploy*
  and every other `argocd app` subcommand that takes an app name, by full name

A query that is an app's exact name, or appears in only one app's name, skips
the picker. Flags after -- go to argocd. Commands marked * ask y/N first, as
does any other command that changes something (deploy asks only before a
manual sync); AW_YES=1 stops the asking.
";

/// Short names, and the full names of `aw`'s own commands.
const SHORT: [(&str, &str); 10] = [
    ("g", "get"),
    ("d", "diff"),
    ("s", "sync"),
    ("l", "logs"),
    ("h", "history"),
    ("m", "manifests"),
    ("w", "wait"),
    ("r", "refresh"),
    ("rs", "restart"),
    ("dp", "deploy"),
];

/// `aw`'s own commands, which are not `argocd app` subcommands.
const OWN: [&str; 3] = ["refresh", "restart", "deploy"];

/// `argocd app` subcommands whose first argument is an app name.
const PASSTHROUGH: [&str; 21] = [
    "get",
    "get-resource",
    "diff",
    "history",
    "logs",
    "manifests",
    "resources",
    "wait",
    "sync",
    "rollback",
    "terminate-op",
    "delete",
    "delete-resource",
    "patch",
    "patch-resource",
    "set",
    "unset",
    "edit",
    "add-source",
    "remove-source",
    "confirm-deletion",
];

/// `argocd app` subcommands that don't take an app name first.
const UNWRAPPED: [&str; 3] = ["create", "list", "actions"];

/// Commands that only read, so they run without asking.
const READ_ONLY: [&str; 9] = [
    "get",
    "get-resource",
    "diff",
    "history",
    "logs",
    "manifests",
    "resources",
    "wait",
    "refresh",
];

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let (words, flags) = match args.iter().position(|a| a == "--") {
        Some(i) => (&args[..i], &args[i + 1..]),
        None => (&args[..], &args[args.len()..]),
    };
    let (command, query) = match words.first().map(String::as_str) {
        Some("-h" | "--help" | "help") => {
            print!("{USAGE}");
            return;
        }
        // What the picker's preview pane runs; see pick::app.
        Some("--preview") => {
            preview(words.get(1).map_or("", String::as_str));
            return;
        }
        Some(word) => match command_name(word) {
            Some(command) => (Some(command), &words[1..]),
            None => (None, words),
        },
        None => (None, words),
    };
    if let Some(flag) = query.iter().find(|w| w.starts_with('-')) {
        usage_error(&format!(
            "argocd flags go after --, as in: aw logs <query> -- {flag}"
        ));
    }
    match (command, flags.is_empty()) {
        (None, false) => usage_error("flags after -- need a command, as in: aw logs <query> -- -f"),
        (Some(c @ ("deploy" | "restart")), false) => {
            usage_error(&format!("{c} takes no argocd flags"))
        }
        _ => {}
    }

    let apps = list_apps();
    let app = find(&apps, &query.join(" "));
    match command {
        None => println!("{}", app.name),
        Some("deploy") => deploy(app),
        Some("restart") => restart(app),
        // aw's view of the app; given flags, argocd's own output instead.
        Some(c @ ("get" | "refresh")) if flags.is_empty() => show(&app.name, c == "refresh"),
        Some("refresh") => exec(&argocd_args(&["app", "get", &app.name, "--refresh"], flags)),
        Some(sub) => {
            let args = argocd_args(&["app", sub, &app.name], flags);
            if !READ_ONLY.contains(&sub) {
                confirm(slice::from_ref(&args), app);
            }
            exec(&args)
        }
    }
}

/// The command `word` names, if any.
fn command_name(word: &str) -> Option<&'static str> {
    if let Some((_, full)) = SHORT.iter().find(|(short, _)| *short == word) {
        return Some(full);
    }
    if UNWRAPPED.contains(&word) {
        usage_error(&format!(
            "aw doesn't wrap `argocd app {word}`; use (aw <query>) for the app name, \
             as in: argocd app actions run (aw <query>) restart …"
        ));
    }
    PASSTHROUGH
        .iter()
        .chain(&OWN)
        .find(|c| **c == word)
        .copied()
}

/// Every app, from `argocd app list -o json`.
fn list_apps() -> Vec<App> {
    let json = argocd_output(&["app", "list", "-o", "json"]);
    apps::parse_list(&json)
        .unwrap_or_else(|e| die(&format!("can't read `argocd app list -o json`: {e}")))
}

/// The app `query` names on its own, or else the one picked starting from it.
fn find<'a>(apps: &'a [App], query: &str) -> &'a App {
    if let Some(app) = apps::resolve(apps, query) {
        return app;
    }
    match pick::app(apps, query) {
        Ok(Some(name)) => apps
            .iter()
            .find(|a| a.name == name)
            .expect("picked from the list"),
        Ok(None) => process::exit(130),
        Err(e) => die(&format!("picker failed: {e}")),
    }
}

/// Refreshes the app, syncs it if it syncs manually and is out of sync, then
/// waits until it is synced and healthy.
fn deploy(app: &App) -> ! {
    let json = argocd_output(&["app", "get", &app.name, "--refresh", "-o", "json"]);
    let now = apps::parse_one(&json)
        .unwrap_or_else(|e| die(&format!("can't read `argocd app get -o json`: {e}")));
    let policy = if now.auto_sync {
        "auto-sync"
    } else {
        "manual sync"
    };
    eprintln!("{}: {}, {} ({policy})", app.name, now.sync, now.health);
    if now.sync == "OutOfSync" && !now.auto_sync {
        let sync = argocd_args(&["app", "sync", &app.name], &[]);
        confirm(slice::from_ref(&sync), app);
        run(&sync);
    }
    exec(&argocd_args(
        &[
            "app",
            "wait",
            &app.name,
            "--sync",
            "--health",
            "--timeout",
            "900",
        ],
        &[],
    ))
}

/// Prints `app` laid out for the picker's preview pane.
fn preview(app: &str) {
    let json = argocd_output(&["app", "get", app, "-o", "json"]);
    // skim sets COLUMNS to the pane's width, its border column included.
    let width = env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse::<usize>().ok())
        .map_or(80, |c| c.saturating_sub(1));
    print_view(&json, width, true);
}

/// Prints `app` as the preview pane shows it, refreshed first if `refresh`,
/// laid out to the terminal's width, in color only on a terminal.
fn show(app: &str, refresh: bool) {
    let mut args = vec!["app", "get", app];
    if refresh {
        args.push("--refresh");
    }
    args.extend(["-o", "json"]);
    let json = argocd_output(&args);
    let width = crossterm::terminal::size().map_or(80, |(columns, _)| usize::from(columns));
    print_view(&json, width, io::stdout().is_terminal());
}

/// Prints the app in `argocd app get -o json` laid out `width` columns wide.
fn print_view(json: &str, width: usize, color: bool) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    match view::render(json, width, now) {
        Ok(text) if color => print!("{text}"),
        Ok(text) => print!("{}", view::plain(&text)),
        Err(e) => die(&format!("can't read `argocd app get -o json`: {e}")),
    }
}

/// Restarts the app's only live workload, or the ones picked.
fn restart(app: &App) -> ! {
    let json = argocd_output(&["app", "get", &app.name, "-o", "json"]);
    let workloads = apps::parse_workloads(&json)
        .unwrap_or_else(|e| die(&format!("can't read `argocd app get -o json`: {e}")));
    let chosen = match workloads.len() {
        0 => die(&format!(
            "{} has no live Deployment, StatefulSet, DaemonSet or Rollout to restart",
            app.name
        )),
        1 => workloads,
        _ => match pick::workloads(&workloads) {
            Ok(picked) if picked.is_empty() => process::exit(130),
            Ok(picked) => picked,
            Err(e) => die(&format!("picker failed: {e}")),
        },
    };
    let commands: Vec<_> = chosen.iter().map(|w| restart_args(app, w)).collect();
    confirm(&commands, app);
    for command in &commands {
        run(command);
    }
    process::exit(0)
}

fn restart_args(app: &App, w: &Workload) -> Vec<String> {
    let mut args = argocd_args(
        &[
            "app", "actions", "run", &app.name, "restart", "--kind", &w.kind,
        ],
        &[],
    );
    for (flag, value) in [("--group", &w.group), ("--namespace", &w.namespace)] {
        if !value.is_empty() {
            args.extend([flag.to_string(), value.clone()]);
        }
    }
    args.extend(["--resource-name".to_string(), w.name.clone()]);
    args
}

/// Shows `commands` and the app's cluster, and asks y/N; exits 1 unless the
/// answer is yes. `AW_YES=1` skips the question.
fn confirm(commands: &[Vec<String>], app: &App) {
    if env::var("AW_YES").is_ok_and(|v| v == "1") {
        return;
    }
    for command in commands {
        eprintln!("argocd {}", command.join(" "));
    }
    let them = if commands.len() == 1 { "it" } else { "them" };
    eprint!("on {}. Run {them}? [y/N] ", app.cluster);
    let _ = io::stderr().flush();
    let mut answer = String::new();
    let _ = io::stdin().lock().read_line(&mut answer);
    if !matches!(answer.trim(), "y" | "Y" | "yes") {
        eprintln!("Not run.");
        process::exit(1);
    }
}

fn argocd_args(args: &[&str], flags: &[String]) -> Vec<String> {
    args.iter()
        .map(|a| a.to_string())
        .chain(flags.iter().cloned())
        .collect()
}

/// Runs `argocd <args>` and returns its stdout; exits with argocd's status if
/// it fails.
fn argocd_output(args: &[&str]) -> String {
    let out = Command::new("argocd")
        .args(args)
        .stderr(Stdio::inherit())
        .output()
        .unwrap_or_else(|e| die(&format!("can't run argocd: {e}")));
    if !out.status.success() {
        process::exit(out.status.code().unwrap_or(1));
    }
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Runs `argocd <args>`; exits with its status if it fails.
fn run(args: &[String]) {
    let status = Command::new("argocd")
        .args(args)
        .status()
        .unwrap_or_else(|e| die(&format!("can't run argocd: {e}")));
    if !status.success() {
        process::exit(status.code().unwrap_or(1));
    }
}

/// Replaces `aw` with `argocd <args>`.
fn exec(args: &[String]) -> ! {
    let e = Command::new("argocd").args(args).exec();
    die(&format!("can't run argocd: {e}"))
}

fn die(message: &str) -> ! {
    eprintln!("aw: {message}");
    process::exit(1)
}

fn usage_error(message: &str) -> ! {
    eprintln!("aw: {message} (see aw --help)");
    process::exit(2)
}
