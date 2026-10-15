//! Runs the `aw` binary against a fake `argocd` that logs its arguments.
//! Every query here names one app, so the picker never opens.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Logs its arguments, prints the fixtures for the JSON calls, and exits with
/// `$AW_TEST_EXIT` for everything but `app list`.
const FAKE_ARGOCD: &str = r#"#!/bin/sh
echo "$*" >> "$AW_TEST_DIR/log"
case "$*" in
  "app list -o json") cat "$AW_TEST_DIR/list.json"; exit 0 ;;
  *"-o json") cat "$AW_TEST_DIR/get.json" ;;
esac
exit "${AW_TEST_EXIT:-0}"
"#;

const LIST: &str = "app list -o json";

struct Fake {
    dir: PathBuf,
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
    /// The argocd calls, in order.
    calls: Vec<String>,
}

impl Fake {
    fn new(test: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("aw-test-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let argocd = dir.join("argocd");
        fs::write(&argocd, FAKE_ARGOCD).unwrap();
        fs::set_permissions(&argocd, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(dir.join("list.json"), include_str!("fixtures/apps.json")).unwrap();
        Fake { dir }
    }

    /// Makes `argocd app get … -o json` report this sync status and policy.
    fn get_returns(self, sync: &str, auto_sync: bool) -> Self {
        let policy = if auto_sync {
            r#","syncPolicy":{"automated":{}}"#
        } else {
            ""
        };
        let json = format!(
            r#"{{"metadata":{{"name":"x"}},"spec":{{"destination":{{"name":"c"}}{policy}}},"status":{{"sync":{{"status":"{sync}"}},"health":{{"status":"Healthy"}}}}}}"#
        );
        fs::write(self.dir.join("get.json"), json).unwrap();
        self
    }

    /// Makes `argocd app get … -o json` report these resources.
    fn get_resources(self, resources: &[&str]) -> Self {
        let json = format!(
            r#"{{"metadata":{{"name":"x"}},"spec":{{}},"status":{{"resources":[{}]}}}}"#,
            resources.join(",")
        );
        fs::write(self.dir.join("get.json"), json).unwrap();
        self
    }

    fn run(&self, args: &[&str], stdin: &str, env: &[(&str, &str)]) -> Run {
        let path = format!("{}:{}", self.dir.display(), std::env::var("PATH").unwrap());
        let mut child = Command::new(env!("CARGO_BIN_EXE_aw"))
            .args(args)
            .env("PATH", path)
            .env("AW_TEST_DIR", &self.dir)
            .env_remove("AW_YES")
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // aw may exit before reading its stdin.
        let _ = child.stdin.take().unwrap().write_all(stdin.as_bytes());
        let out = child.wait_with_output().unwrap();
        let log = fs::read_to_string(self.dir.join("log")).unwrap_or_default();
        Run {
            code: out.status.code().unwrap(),
            stdout: String::from_utf8(out.stdout).unwrap(),
            stderr: String::from_utf8(out.stderr).unwrap(),
            calls: log.lines().map(String::from).collect(),
        }
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn bare_query_prints_the_app_it_names() {
    let r = Fake::new("bare").run(&["audit-log"], "", &[]);
    assert_eq!((r.code, r.stdout.as_str()), (0, "audit-log-team-apps\n"));
    assert_eq!(r.calls, [LIST]);
}

#[test]
fn short_names_pass_through_with_flags() {
    let r = Fake::new("logs").run(&["l", "audit-log", "--", "-f", "--tail", "100"], "", &[]);
    assert_eq!(r.code, 0);
    assert_eq!(
        r.calls,
        [LIST, "app logs audit-log-team-apps -f --tail 100"]
    );
}

#[test]
fn full_names_pass_through() {
    let r = Fake::new("history").run(&["history", "import-prod"], "", &[]);
    assert_eq!(r.calls, [LIST, "app history jobs-worker-import-prod"]);
}

#[test]
fn argocds_exit_code_comes_back() {
    let r = Fake::new("exit").run(&["d", "audit-log"], "", &[("AW_TEST_EXIT", "20")]);
    assert_eq!(r.code, 20);
    assert_eq!(r.calls, [LIST, "app diff audit-log-team-apps"]);
}

#[test]
fn sync_asks_first_and_runs_on_yes() {
    let r = Fake::new("sync-yes").run(&["s", "import-prod"], "y\n", &[]);
    assert_eq!(r.code, 0);
    assert!(
        r.stderr
            .contains("argocd app sync jobs-worker-import-prod\non prod-us-1. Run it? [y/N]")
    );
    assert_eq!(r.calls, [LIST, "app sync jobs-worker-import-prod"]);
}

#[test]
fn sync_runs_nothing_on_no_or_without_an_answer() {
    for answer in ["n\n", ""] {
        let r = Fake::new("sync-no").run(&["s", "import-prod"], answer, &[]);
        assert_eq!(r.code, 1);
        assert_eq!(r.calls, [LIST]);
    }
}

#[test]
fn aw_yes_skips_the_question() {
    let r = Fake::new("aw-yes").run(&["s", "import-prod"], "", &[("AW_YES", "1")]);
    assert!(!r.stderr.contains("[y/N]"));
    assert_eq!(r.calls, [LIST, "app sync jobs-worker-import-prod"]);
}

#[test]
fn refresh_passes_flags_to_get_without_asking() {
    let r = Fake::new("refresh").run(&["r", "audit-log", "--", "--hard-refresh"], "", &[]);
    assert!(!r.stderr.contains("[y/N]"));
    assert_eq!(
        r.calls,
        [LIST, "app get audit-log-team-apps --refresh --hard-refresh"]
    );
}

#[test]
fn get_and_refresh_print_the_app_as_the_pane_shows_it() {
    for (command, call) in [
        ("g", "app get audit-log-team-apps -o json"),
        ("r", "app get audit-log-team-apps --refresh -o json"),
    ] {
        let fake = Fake::new("show");
        fs::write(fake.dir.join("get.json"), include_str!("fixtures/app.json")).unwrap();
        let r = fake.run(&[command, "audit-log"], "", &[]);
        assert_eq!(r.code, 0, "{command}");
        assert_eq!(r.calls, [LIST, call]);
        // The fake reports the fixture's app. Its stdout is no terminal, so
        // the icons come without colors.
        assert!(
            r.stdout
                .starts_with("shop-api-staging-2-orders\n\u{f0aa} OutOfSync  \u{ef4f} Degraded"),
            "{command}: {}",
            r.stdout
        );
        assert!(!r.stdout.contains('\x1b'), "{command}");
    }
}

#[test]
fn get_with_flags_runs_argocds_own_get() {
    let r = Fake::new("get-flags").run(&["g", "audit-log", "--", "-o", "yaml"], "", &[]);
    assert_eq!(r.code, 0);
    assert_eq!(r.calls, [LIST, "app get audit-log-team-apps -o yaml"]);
}

#[test]
fn deploy_waits_on_an_auto_sync_app_without_asking() {
    let r = Fake::new("dp-auto")
        .get_returns("OutOfSync", true)
        .run(&["dp", "import-dev"], "", &[]);
    assert_eq!(r.code, 0);
    assert!(!r.stderr.contains("[y/N]"));
    assert_eq!(
        r.calls,
        [
            LIST,
            "app get jobs-worker-import-dev --refresh -o json",
            "app wait jobs-worker-import-dev --sync --health --timeout 900",
        ]
    );
}

const REFRESH_PROD: &str = "app get jobs-worker-import-prod --refresh -o json";
const SYNC_PROD: &str = "app sync jobs-worker-import-prod";
const WAIT_PROD: &str = "app wait jobs-worker-import-prod --sync --health --timeout 900";

#[test]
fn deploy_asks_before_syncing_a_manual_app() {
    let r =
        Fake::new("dp-yes")
            .get_returns("OutOfSync", false)
            .run(&["dp", "import-prod"], "y\n", &[]);
    assert_eq!(r.code, 0);
    assert!(
        r.stderr
            .contains("argocd app sync jobs-worker-import-prod\non prod-us-1. Run it? [y/N]")
    );
    assert_eq!(r.calls, [LIST, REFRESH_PROD, SYNC_PROD, WAIT_PROD]);
}

#[test]
fn deploy_stops_when_the_sync_is_declined() {
    let r =
        Fake::new("dp-no")
            .get_returns("OutOfSync", false)
            .run(&["dp", "import-prod"], "n\n", &[]);
    assert_eq!(r.code, 1);
    assert_eq!(r.calls, [LIST, REFRESH_PROD]);
}

#[test]
fn deploy_only_waits_when_there_is_nothing_to_sync() {
    let r =
        Fake::new("dp-synced")
            .get_returns("Synced", false)
            .run(&["dp", "import-prod"], "", &[]);
    assert!(!r.stderr.contains("[y/N]"));
    assert_eq!(r.calls, [LIST, REFRESH_PROD, WAIT_PROD]);
}

#[test]
fn restart_asks_then_restarts_the_only_live_workload() {
    let r = Fake::new("rs")
        .get_resources(&[
            r#"{"kind":"Service","namespace":"team-apps-audit-log","name":"audit-log","health":{"status":"Healthy"}}"#,
            r#"{"group":"apps","kind":"Deployment","namespace":"team-apps-audit-log","name":"audit-log","health":{"status":"Healthy"}}"#,
        ])
        .run(&["rs", "audit-log"], "y\n", &[]);
    let restart = "app actions run audit-log-team-apps restart --kind Deployment --group apps \
                   --namespace team-apps-audit-log --resource-name audit-log";
    assert_eq!(r.code, 0);
    assert!(
        r.stderr
            .contains(&format!("argocd {restart}\non prod-eu-1. Run it? [y/N]"))
    );
    assert_eq!(
        r.calls,
        [LIST, "app get audit-log-team-apps -o json", restart]
    );
}

#[test]
fn restart_needs_a_live_workload() {
    // A KEDA ScaledJob, and a Deployment that is in git but not live.
    for resource in [
        r#"{"group":"keda.sh","kind":"ScaledJob","namespace":"jobs","name":"jobs-worker-import","health":{"status":"Healthy"}}"#,
        r#"{"group":"apps","kind":"Deployment","namespace":"jobs","name":"jobs-worker-import","health":{"status":"Missing"}}"#,
    ] {
        let r =
            Fake::new("rs-none")
                .get_resources(&[resource])
                .run(&["rs", "import-prod"], "y\n", &[]);
        assert_eq!(r.code, 1);
        assert_eq!(r.calls, [LIST, "app get jobs-worker-import-prod -o json"]);
    }
}

#[test]
fn preview_lays_out_app_get_for_the_pane() {
    let fake = Fake::new("preview");
    fs::write(fake.dir.join("get.json"), include_str!("fixtures/app.json")).unwrap();
    // skim's COLUMNS counts the pane's border column.
    let r = fake.run(&["--preview", "shop-api"], "", &[("COLUMNS", "41")]);
    assert_eq!(r.code, 0);
    assert_eq!(r.calls, ["app get shop-api -o json"]);
    let widest = r
        .stdout
        .lines()
        .map(|l| without_colors(l).chars().count())
        .max();
    assert_eq!(widest, Some(40));
}

/// `text` without its SGR color codes.
fn without_colors(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find('\x1b') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        rest = &rest[rest.find('m').unwrap() + 1..];
    }
    out + rest
}

#[test]
fn bad_command_lines_exit_2_before_calling_argocd() {
    let cases: [&[&str]; 4] = [
        &["actions", "audit-log"],
        &["l", "audit-log", "-f"],
        &["dp", "audit-log", "--", "--timeout", "60"],
        &["audit-log", "--", "-f"],
    ];
    for args in cases {
        let r = Fake::new("usage").run(args, "", &[]);
        assert_eq!(r.code, 2, "{args:?}");
        assert!(r.calls.is_empty(), "{args:?}");
    }
}
