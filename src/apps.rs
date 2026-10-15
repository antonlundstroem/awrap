//! The Argo CD Applications `aw` works on, read from `argocd app list -o json`.

use serde::Deserialize;

/// Workload kinds that have Argo CD's built-in `restart` action.
const RESTARTABLE: [&str; 4] = ["Deployment", "StatefulSet", "DaemonSet", "Rollout"];

pub struct App {
    pub name: String,
    /// `.spec.destination.name`, or `.server` for an app that targets a URL.
    pub cluster: String,
    pub sync: String,
    pub health: String,
    pub auto_sync: bool,
}

#[derive(Clone)]
pub struct Workload {
    pub group: String,
    pub kind: String,
    pub namespace: String,
    pub name: String,
}

/// Parses `argocd app list -o json`.
pub fn parse_list(json: &str) -> serde_json::Result<Vec<App>> {
    let raw: Vec<RawApp> = serde_json::from_str(json)?;
    Ok(raw.into_iter().map(App::from).collect())
}

/// Parses `argocd app get -o json`.
pub fn parse_one(json: &str) -> serde_json::Result<App> {
    serde_json::from_str::<RawApp>(json).map(App::from)
}

/// The live workloads `restart` can act on, from `argocd app get -o json`.
/// `app list` won't do: it leaves out resource health, which tells what is
/// live (see FINDINGS.md).
pub fn parse_workloads(json: &str) -> serde_json::Result<Vec<Workload>> {
    let raw: RawApp = serde_json::from_str(json)?;
    Ok(raw
        .status
        .resources
        .into_iter()
        .filter(|r| {
            RESTARTABLE.contains(&r.kind.as_str())
                && r.health.as_ref().is_none_or(|h| h.status != "Missing")
        })
        .map(|r| Workload {
            group: r.group,
            kind: r.kind,
            namespace: r.namespace,
            name: r.name,
        })
        .collect())
}

/// The app `query` names on its own: an exact name, or the only name that
/// contains it.
pub fn resolve<'a>(apps: &'a [App], query: &str) -> Option<&'a App> {
    if query.is_empty() {
        return None;
    }
    if let Some(app) = apps.iter().find(|a| a.name == query) {
        return Some(app);
    }
    let mut hits = apps.iter().filter(|a| a.name.contains(query));
    match (hits.next(), hits.next()) {
        (Some(app), None) => Some(app),
        _ => None,
    }
}

// The part of an Application that `aw` reads.

#[derive(Deserialize)]
struct RawApp {
    metadata: RawMetadata,
    spec: RawSpec,
    #[serde(default)]
    status: RawStatus,
}

#[derive(Deserialize)]
struct RawMetadata {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSpec {
    #[serde(default)]
    destination: RawDestination,
    sync_policy: Option<RawSyncPolicy>,
}

#[derive(Deserialize, Default)]
struct RawDestination {
    #[serde(default)]
    name: String,
    #[serde(default)]
    server: String,
}

#[derive(Deserialize)]
struct RawSyncPolicy {
    automated: Option<RawAutomated>,
}

#[derive(Deserialize)]
struct RawAutomated {
    /// Argo CD 3 can turn auto-sync off with `enabled: false`.
    enabled: Option<bool>,
}

#[derive(Deserialize, Default)]
struct RawStatus {
    #[serde(default)]
    sync: RawState,
    #[serde(default)]
    health: RawState,
    #[serde(default)]
    resources: Vec<RawResource>,
}

#[derive(Deserialize, Default)]
struct RawState {
    #[serde(default)]
    status: String,
}

#[derive(Deserialize)]
struct RawResource {
    #[serde(default)]
    group: String,
    kind: String,
    #[serde(default)]
    namespace: String,
    name: String,
    /// `Missing` for a resource that is in git but not live. `app get` fills
    /// it in; `app list` leaves it out.
    health: Option<RawState>,
}

impl From<RawApp> for App {
    fn from(raw: RawApp) -> Self {
        let dest = raw.spec.destination;
        let auto_sync = raw
            .spec
            .sync_policy
            .and_then(|p| p.automated)
            .is_some_and(|a| a.enabled != Some(false));
        App {
            name: raw.metadata.name,
            cluster: if dest.name.is_empty() {
                dest.server
            } else {
                dest.name
            },
            sync: raw.status.sync.status,
            health: raw.status.health.status,
            auto_sync,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<App> {
        parse_list(include_str!("../tests/fixtures/apps.json")).unwrap()
    }

    fn app<'a>(apps: &'a [App], name: &str) -> &'a App {
        apps.iter().find(|a| a.name == name).unwrap()
    }

    #[test]
    fn cluster_is_the_destination_name_or_else_the_server() {
        let apps = fixture();
        assert_eq!(app(&apps, "audit-log-team-apps").cluster, "prod-eu-1");
        assert_eq!(
            app(&apps, "platform-tools").cluster,
            "https://kubernetes.default.svc"
        );
    }

    #[test]
    fn auto_sync_needs_automated_without_enabled_false() {
        let apps = fixture();
        assert!(app(&apps, "jobs-worker-import-dev").auto_sync);
        assert!(!app(&apps, "jobs-worker-import-prod").auto_sync);
        assert!(!app(&apps, "paused-example").auto_sync);
    }

    #[test]
    fn workloads_are_the_live_restartable_ones() {
        let workloads = parse_workloads(include_str!("../tests/fixtures/app.json")).unwrap();
        let names: Vec<_> = workloads.iter().map(|w| w.name.as_str()).collect();
        // Not shop-api-cron, a Deployment in git but not live, nor the
        // ScaledJob or the Job.
        assert_eq!(names, ["shop-api", "shop-api-worker", "bg-shop-api"]);
    }

    #[test]
    fn resolve_takes_an_exact_name_or_a_unique_substring() {
        let apps = fixture();
        let name = |q| resolve(&apps, q).map(|a| a.name.as_str());
        assert_eq!(name("platform-tools"), Some("platform-tools"));
        assert_eq!(name("audit-log"), Some("audit-log-team-apps"));
        assert_eq!(name("import-prod"), Some("jobs-worker-import-prod"));
        assert_eq!(name("jobs"), None);
        assert_eq!(name("jobs import"), None);
        assert_eq!(name("nothing-like-this"), None);
        assert_eq!(name(""), None);
    }
}
