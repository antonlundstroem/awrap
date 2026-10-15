//! The picker's preview pane: one app as `argocd app get -o json` reports it,
//! laid out to the pane's width, statuses in color and with Argo CD's icons,
//! problems first.

use std::fmt::Write;

use serde::Deserialize;

// SGR parameters.
const PLAIN: &str = "";
const BOLD: &str = "1";
const DIM: &str = "2";
const FG: [&str; 8] = ["30", "31", "32", "33", "34", "35", "36", "37"];

/// Labels, after their icons, are padded to this width, and values that wrap
/// are indented by it.
const LABEL: usize = 13;

// Icons are Nerd Font glyphs (Nerd Fonts 3.2 or newer). Field labels and
// their icons:
const CLUSTER: (char, &str) = ('\u{f10fe}', "cluster"); // kubernetes
const NAMESPACE: (char, &str) = ('\u{ea8b}', "namespace"); // symbol-namespace
const PROJECT: (char, &str) = ('\u{f07b}', "project"); // folder
const REPO: (char, &str) = ('\u{f1d3}', "repo"); // git
const PATH: (char, &str) = ('\u{f07c}', "path"); // folder-open
const REVISION: (char, &str) = ('\u{f126}', "revision"); // code-branch
const LAST_SYNC: (char, &str) = ('\u{f1da}', "last sync"); // clock-rotate-left
const IMAGES: (char, &str) = ('\u{f1b2}', "images"); // cube
const URLS: (char, &str) = ('\u{f0c1}', "urls"); // link
/// A field's further lines.
const MORE: (char, &str) = (' ', "");

const RESOURCES: char = '\u{f1b3}'; // cubes
/// Argo CD's UI marks a resource that a sync would prune with a trash can.
const PRUNE: char = '\u{f1f8}';
const ERROR: char = '\u{f057}'; // circle-xmark
const WARNING: char = '\u{f071}'; // triangle-exclamation

/// Resource health from worst to best; resources sort in this order.
const WORST_FIRST: [&str; 5] = ["Degraded", "Missing", "Progressing", "Suspended", "Unknown"];

/// The Font Awesome icon Argo CD's UI gives a sync, health or operation
/// status. Unknown, of either, gets a question mark.
pub fn icon(status: &str) -> char {
    match status {
        "Synced" | "Succeeded" => '\u{f058}', // circle-check
        "OutOfSync" => '\u{f0aa}',            // circle-arrow-up
        "Healthy" => '\u{f004}',              // heart
        "Degraded" => '\u{ef4f}',             // heart-crack
        "Missing" => '\u{eefe}',              // ghost
        "Suspended" => '\u{f28b}',            // circle-pause
        "Progressing" | "Running" | "Terminating" => '\u{f1ce}', // circle-notch
        "Failed" | "Error" => ERROR,
        _ => '\u{f059}', // circle-question
    }
}

/// `status` after its icon.
fn with_icon(status: &str) -> String {
    format!("{} {status}", icon(status))
}

/// The ANSI color (0–7) Argo CD's UI gives a sync, health or operation
/// status: green when fine, yellow when out of sync or missing, red when
/// broken, cyan while under way, magenta when suspended.
pub fn color(status: &str) -> Option<u8> {
    match status {
        "Synced" | "Healthy" | "Succeeded" => Some(2),
        "OutOfSync" | "Missing" => Some(3),
        "Degraded" | "Failed" | "Error" | "Terminating" => Some(1),
        "Progressing" | "Running" => Some(6),
        "Suspended" => Some(5),
        _ => None,
    }
}

fn tone(status: &str) -> &'static str {
    color(status).map_or(PLAIN, |c| FG[usize::from(c)])
}

/// Lays out the app in `json` for a pane `width` columns wide. `now` is the
/// Unix time that "3h ago" counts back from.
pub fn render(json: &str, width: usize, now: u64) -> serde_json::Result<String> {
    let app: App = serde_json::from_str(json)?;
    let (spec, status) = (&app.spec, &app.status);
    let mut out = Out {
        text: String::new(),
        width,
    };

    out.line(&[(&app.metadata.name, BOLD)], 0);
    let (sync, health) = (status.sync.status.as_str(), status.health.status.as_str());
    out.line(
        &[
            (&with_icon(sync), tone(sync)),
            ("  ", PLAIN),
            (&with_icon(health), tone(health)),
            ("  ", PLAIN),
            (&policy(spec), DIM),
        ],
        0,
    );
    out.text.push('\n');

    let dest = &spec.destination;
    let cluster = if dest.name.is_empty() {
        &dest.server
    } else {
        &dest.name
    };
    out.field(CLUSTER, &[(cluster, PLAIN)]);
    out.field(NAMESPACE, &[(&dest.namespace, PLAIN)]);
    out.field(PROJECT, &[(&spec.project, PLAIN)]);
    out.field(REPO, &[(&spec.source.repo_url, PLAIN)]);
    out.field(PATH, &[(&spec.source.path, PLAIN)]);
    out.field(REVISION, &[(&revision(spec, status), PLAIN)]);
    if let Some(op) = &status.operation_state {
        last_sync(&mut out, op, now);
    }
    for (i, image) in status.summary.images.iter().enumerate() {
        out.field(if i == 0 { IMAGES } else { MORE }, &[(image, PLAIN)]);
    }
    for (i, url) in status.summary.external_urls.iter().enumerate() {
        out.field(if i == 0 { URLS } else { MORE }, &[(url, PLAIN)]);
    }
    conditions(&mut out, &status.conditions);
    resources(&mut out, &status.resources, &dest.namespace);
    Ok(out.text)
}

/// "auto-sync, prune, self-heal", or "manual sync".
fn policy(spec: &Spec) -> String {
    match &spec.sync_policy.automated {
        // Argo CD 3 can turn auto-sync off with `enabled: false`.
        Some(auto) if auto.enabled != Some(false) => {
            let mut text = String::from("auto-sync");
            for (on, name) in [(auto.prune, ", prune"), (auto.self_heal, ", self-heal")] {
                if on {
                    text.push_str(name);
                }
            }
            text
        }
        _ => "manual sync".to_string(),
    }
}

/// The target revision, and the commit it resolved to.
fn revision(spec: &Spec, status: &Status) -> String {
    let target = match spec.source.target_revision.as_str() {
        "" => "HEAD",
        target => target,
    };
    match short(&status.sync.revision) {
        "" => target.to_string(),
        commit => format!("{target} ({commit})"),
    }
}

/// A commit SHA cut to 7 characters; any other revision as it is.
fn short(revision: &str) -> &str {
    if revision.len() == 40 && revision.bytes().all(|b| b.is_ascii_hexdigit()) {
        &revision[..7]
    } else {
        revision
    }
}

/// How the last sync went, when, at which commit and on whose request, and
/// the message when it didn't succeed.
fn last_sync(out: &mut Out, op: &LastOperation, now: u64) {
    let at = if op.finished_at.is_empty() {
        &op.started_at
    } else {
        &op.finished_at
    };
    let mut about = format!(" {}", ago(at, now));
    let commit = short(&op.sync_result.revision);
    if !commit.is_empty() {
        let _ = write!(about, " at {commit}");
    }
    let by = &op.operation.initiated_by;
    if by.automated {
        about.push_str(" by auto-sync");
    } else if !by.username.is_empty() {
        let _ = write!(about, " by {}", by.username);
    }
    out.field(
        LAST_SYNC,
        &[(&with_icon(&op.phase), tone(&op.phase)), (&about, PLAIN)],
    );
    if op.phase != "Succeeded" {
        out.field(MORE, &[(op.message.trim(), PLAIN)]);
    }
}

/// The conditions by type: errors in red, warnings (every other type) in
/// yellow. Up to three messages of an error type are shown and one of a
/// warning type, which come many to an app and alike; the rest are counted.
fn conditions(out: &mut Out, conditions: &[Condition]) {
    let mut types: Vec<&str> = Vec::new();
    for c in conditions {
        if !types.contains(&c.kind.as_str()) {
            types.push(&c.kind);
        }
    }
    for kind in types {
        let messages: Vec<&str> = conditions
            .iter()
            .filter(|c| c.kind == kind)
            .map(|c| c.message.trim())
            .collect();
        let (icon, style, shown) = if kind.ends_with("Error") {
            (ERROR, FG[1], 3)
        } else {
            (WARNING, FG[3], 1)
        };
        let title = format!("{icon} {kind}");
        let count = match messages.len() {
            1 => String::new(),
            n => format!(" ×{n}"),
        };
        out.text.push('\n');
        out.line(&[(&title, style), (&count, DIM)], 0);
        for message in messages.iter().take(shown) {
            out.line(&[("  ", PLAIN), (message, PLAIN)], 2);
        }
        if messages.len() > shown {
            let more = format!("  … {} more", messages.len() - shown);
            out.line(&[(&more, DIM)], 2);
        }
    }
}

/// The resources, problems first: worst health first, then OutOfSync before
/// Synced. Each row starts with the sync and health icons Argo CD's UI gives
/// it. Resources that are neither healthy nor missing show their health
/// message; those outside the app's namespace show theirs.
fn resources(out: &mut Out, resources: &[Resource], namespace: &str) {
    if resources.is_empty() {
        return;
    }
    let mut sorted: Vec<&Resource> = resources.iter().collect();
    sorted.sort_by_key(|r| {
        let worse = WORST_FIRST.iter().position(|h| *h == r.health_status());
        (worse.unwrap_or(WORST_FIRST.len()), r.status != "OutOfSync")
    });

    let mut header = vec![
        (format!("{RESOURCES} Resources"), BOLD),
        (format!(" {}", resources.len()), PLAIN),
    ];
    let out_of_sync = resources.iter().filter(|r| r.status == "OutOfSync").count();
    let unhealthy = WORST_FIRST.map(|h| {
        let n = resources.iter().filter(|r| r.health_status() == h).count();
        (n, h)
    });
    for (n, status) in [(out_of_sync, "OutOfSync")].into_iter().chain(unhealthy) {
        if n > 0 {
            // No-break spaces: the line may only break before a "·".
            let count = format!(" ·\u{a0}{}\u{a0}{n}\u{a0}{status}", icon(status));
            header.push((count, tone(status)));
        }
    }
    out.text.push('\n');
    let header: Vec<(&str, &str)> = header.iter().map(|(t, s)| (t.as_str(), *s)).collect();
    out.line(&header, 0);

    // The two icons and a space after each.
    let indent = 4;
    for r in sorted {
        let health = r.health_status();
        let sync_icon = if r.requires_pruning {
            PRUNE
        } else {
            icon(&r.status)
        };
        let sync_cell = format!("{sync_icon} ");
        let health_cell = match health {
            "" => "  ".to_string(),
            health => format!("{} ", icon(health)),
        };
        let name = format!("{}/{}", r.kind, r.name);
        let elsewhere = if r.namespace.is_empty() || r.namespace == namespace {
            String::new()
        } else {
            format!(" in {}", r.namespace)
        };
        let mut runs = vec![
            (sync_cell.as_str(), tone(&r.status)),
            (&health_cell, tone(health)),
            (&name, PLAIN),
            (&elsewhere, DIM),
        ];
        if r.requires_pruning {
            runs.push((" requires pruning", DIM));
        }
        if r.hook {
            runs.push((" hook", DIM));
        }
        out.line(&runs, indent);
        if let Some(h) = &r.health
            && !matches!(h.status.as_str(), "Healthy" | "Missing")
            && !h.message.trim().is_empty()
        {
            let pad = " ".repeat(indent);
            out.line(&[(&pad, PLAIN), (h.message.trim(), DIM)], indent);
        }
    }
}

/// "45s ago", "12m ago", "3h ago" or "5d ago" for a Kubernetes timestamp such
/// as "2026-06-17T19:20:20Z"; the timestamp itself if it doesn't parse.
fn ago(time: &str, now: u64) -> String {
    let Some(then) = unix_time(time) else {
        return time.to_string();
    };
    let s = now.saturating_sub(then);
    match s {
        0..60 => format!("{s}s ago"),
        60..3600 => format!("{}m ago", s / 60),
        3600..86400 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86400),
    }
}

/// Seconds since 1970 for "YYYY-MM-DDTHH:MM:SSZ".
fn unix_time(time: &str) -> Option<u64> {
    let num = |at: usize, len: usize| time.get(at..at + len)?.parse::<i64>().ok();
    let (y, m, d) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    let (hour, min, sec) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
    // Days since 1970-01-01: Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let year_of_era = y - era * 400;
    let day_of_year = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    u64::try_from(days * 86_400 + hour * 3600 + min * 60 + sec).ok()
}

/// The rendered text, and the width it is laid out to.
struct Out {
    text: String,
    width: usize,
}

impl Out {
    /// A dim icon and label padded to `LABEL`, then `runs`; nothing if `runs`
    /// are empty.
    fn field(&mut self, (icon, label): (char, &str), runs: &[(&str, &str)]) {
        if runs.iter().all(|(text, _)| text.is_empty()) {
            return;
        }
        let label = format!("{:LABEL$}", format!("{icon} {label}"));
        let mut line = vec![(label.as_str(), DIM)];
        line.extend_from_slice(runs);
        self.line(&line, LABEL);
    }

    /// Writes `runs`, each a text and its SGR style, as lines that fit the
    /// width. A line breaks at its last space that fits, past the first
    /// `indent` columns, or mid-word if there is none; the lines after it
    /// start `indent` columns in, or half the width if that is less. A newline
    /// in a text starts an indented line.
    fn line(&mut self, runs: &[(&str, &str)], indent: usize) {
        let hang = indent.min(self.width / 2);
        let chars: Vec<(char, &str)> = runs
            .iter()
            .flat_map(|&(text, style)| text.chars().map(move |c| (c, style)))
            .collect();
        let mut first = true;
        for paragraph in chars.split(|&(c, _)| c == '\n') {
            let mut rest = paragraph;
            loop {
                let (room, start) = if first {
                    (self.width, indent)
                } else {
                    (self.width - hang, 0)
                };
                let (now, next) = if rest.len() <= room {
                    rest.split_at(rest.len())
                } else {
                    match rest[..=room].iter().rposition(|&(c, _)| c == ' ') {
                        Some(at) if at > start => (&rest[..at], &rest[at + 1..]),
                        _ => rest.split_at(room.max(1)),
                    }
                };
                if !first {
                    self.text.push_str(&" ".repeat(hang));
                }
                self.styled(now);
                self.text.push('\n');
                first = false;
                rest = next;
                if rest.is_empty() {
                    break;
                }
            }
        }
    }

    /// Writes `chars`, each run of one style between its SGR codes.
    fn styled(&mut self, chars: &[(char, &str)]) {
        for run in chars.chunk_by(|a, b| a.1 == b.1) {
            let style = run[0].1;
            if !style.is_empty() {
                let _ = write!(self.text, "\x1b[{style}m");
            }
            self.text.extend(run.iter().map(|&(c, _)| c));
            if !style.is_empty() {
                self.text.push_str("\x1b[0m");
            }
        }
    }
}

// The part of `argocd app get -o json` shown here.

#[derive(Deserialize)]
struct App {
    metadata: Metadata,
    #[serde(default)]
    spec: Spec,
    #[serde(default)]
    status: Status,
}

#[derive(Deserialize)]
struct Metadata {
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Spec {
    project: String,
    destination: Destination,
    source: Source,
    sync_policy: SyncPolicy,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Destination {
    name: String,
    server: String,
    namespace: String,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Source {
    #[serde(rename = "repoURL")]
    repo_url: String,
    path: String,
    target_revision: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SyncPolicy {
    automated: Option<Automated>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Automated {
    enabled: Option<bool>,
    prune: bool,
    self_heal: bool,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Status {
    sync: SyncStatus,
    health: Health,
    operation_state: Option<LastOperation>,
    conditions: Vec<Condition>,
    summary: Summary,
    resources: Vec<Resource>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SyncStatus {
    status: String,
    revision: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Health {
    status: String,
    message: String,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct LastOperation {
    phase: String,
    message: String,
    started_at: String,
    finished_at: String,
    operation: Operation,
    sync_result: SyncResult,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Operation {
    initiated_by: InitiatedBy,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct InitiatedBy {
    username: String,
    automated: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SyncResult {
    revision: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Condition {
    #[serde(rename = "type")]
    kind: String,
    message: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Summary {
    images: Vec<String>,
    #[serde(rename = "externalURLs")]
    external_urls: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Resource {
    kind: String,
    namespace: String,
    name: String,
    /// The sync status.
    status: String,
    health: Option<Health>,
    requires_pruning: bool,
    hook: bool,
}

impl Resource {
    fn health_status(&self) -> &str {
        self.health.as_ref().map_or("", |h| h.status.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP: &str = include_str!("../tests/fixtures/app.json");

    /// 2026-10-15T08:20:20Z, two hours after the fixture's last sync.
    const NOW: u64 = 1_792_052_420;

    /// `text` without its SGR codes.
    fn plain(text: &str) -> String {
        let mut out = String::new();
        let mut rest = text;
        while let Some(at) = rest.find('\x1b') {
            out.push_str(&rest[..at]);
            rest = &rest[at..];
            rest = &rest[rest.find('m').unwrap() + 1..];
        }
        out + rest
    }

    /// `text` with its icons spelled out, so the layout reads in an editor.
    fn named(text: &str) -> String {
        let names = [
            (CLUSTER.0, "k8s"),
            (NAMESPACE.0, "ns"),
            (PROJECT.0, "folder"),
            (REPO.0, "git"),
            (PATH.0, "open"),
            (REVISION.0, "branch"),
            (LAST_SYNC.0, "history"),
            (IMAGES.0, "cube"),
            (URLS.0, "link"),
            (RESOURCES, "cubes"),
            (PRUNE, "trash"),
            (ERROR, "x"),
            (WARNING, "warn"),
            (icon("Synced"), "ok"),
            (icon("OutOfSync"), "up"),
            (icon("Healthy"), "heart"),
            (icon("Degraded"), "broken"),
            (icon("Missing"), "ghost"),
            (icon("Suspended"), "pause"),
            (icon("Progressing"), "spin"),
            (icon("Unknown"), "?"),
        ];
        text.chars()
            .map(|c| match names.iter().find(|(icon, _)| *icon == c) {
                Some((_, name)) => format!("[{name}]"),
                None => c.to_string(),
            })
            .collect()
    }

    #[test]
    fn renders_an_app_in_trouble() {
        let expected = "\
shop-api-staging-2-orders
[up] OutOfSync  [broken] Degraded  manual sync

[k8s] cluster    staging-2
[ns] namespace  shop-api
[folder] project    orders
[git] repo       https://git.example.com/shop/deploy.git
[open] path       k8s/shop-api/qa/staging-2
[branch] revision   HEAD (f012857)
[history] last sync  [x] Failed 2h ago at f4f2324 by someone@example.com
             one or more objects failed to apply, reason:
             admission webhook
             \"validate.nginx.ingress.kubernetes.io\" denied the
             request:
             nginx.ingress.kubernetes.io/configuration-snippet
             annotation cannot be used. Snippet directives are
             disabled by the Ingress administrator
[cube] images     registry.example.com/shop/shop-api:11.4.3
             nginx:1.27-alpine
[link] urls       https://shop.staging.example.com/

[x] SyncError
  Failed sync attempt to
  f4f2324178d1644d2873b9df144f712effd3fa55: one or more objects
  failed to apply

[warn] SharedResourceWarning ×4
  ConfigMap/nginx-config is part of applications
  argocd/shop-api-staging-2-orders and shop-web-staging-2-orders
  … 3 more

[cubes] Resources 11 ·\u{a0}[up]\u{a0}4\u{a0}OutOfSync ·\u{a0}[broken]\u{a0}1\u{a0}Degraded ·\u{a0}[ghost]\u{a0}1\u{a0}Missing
·\u{a0}[pause]\u{a0}1\u{a0}Suspended
[up] [broken] Deployment/shop-api-worker
    Deployment \"shop-api-worker\" exceeded its progress deadline
[up] [ghost] Deployment/shop-api-cron
[ok] [pause] Rollout/bg-shop-api
    BlueGreenPause
[trash]   ConfigMap/shop-api-old requires pruning
[up] [heart] Ingress/shop-api
[ok]   ConfigMap/nginx-config
[ok] [heart] Service/shop-api
[ok] [heart] Deployment/shop-api
[ok] [heart] Job/shop-api-migrate hook
[ok] [heart] ScaledJob/shop-api-import
[ok]   ServiceMonitor/shop-api in monitoring
";
        assert_eq!(named(&plain(&render(APP, 64, NOW).unwrap())), expected);
    }

    #[test]
    fn no_line_is_wider_than_the_pane() {
        for width in [30, 40, 64, 100] {
            for line in plain(&render(APP, width, NOW).unwrap()).lines() {
                assert!(line.chars().count() <= width, "{width}: {line:?}");
            }
        }
    }

    #[test]
    fn statuses_are_colored_after_their_icons() {
        let text = render(APP, 64, NOW).unwrap();
        for (status, color) in [("OutOfSync", 33), ("Degraded", 31), ("Failed", 31)] {
            let colored = format!("\x1b[{color}m{} {status}", icon(status));
            assert!(text.contains(&colored), "{status}");
        }
    }

    #[test]
    fn ago_counts_back_from_now() {
        let now = unix_time("2026-10-15T12:00:00Z").unwrap();
        assert_eq!(now, 1_792_065_600);
        assert_eq!(unix_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(ago("2026-10-15T11:59:15Z", now), "45s ago");
        assert_eq!(ago("2026-10-15T11:48:00Z", now), "12m ago");
        assert_eq!(ago("2026-10-15T09:00:00Z", now), "3h ago");
        assert_eq!(ago("2026-10-10T12:00:00Z", now), "5d ago");
        assert_eq!(ago("yesterday", now), "yesterday");
    }
}
