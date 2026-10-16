# Findings: driving Argo CD from the terminal

Researched 2026-10-14 against the Argo CD docs (`stable`, ≈ v3.5) and source
(`master`), and checked with the `argocd` CLI v3.4.6 against a v3.5.3 server
of about 600 apps, running only read-only commands. Notes about a particular
instance go in the gitignored `FINDINGS.local.md`. Sources at the end.

## Clusters in the argocd CLI

- The CLI has no "current cluster". `argocd context` switches Argo CD
  *servers*, not destination clusters.
- App commands (`get`, `diff`, `sync`, `wait`, `logs`, `actions run`,
  `history`, `rollback`, `manifests`) take an app name and have no cluster
  flag. An app name already identifies one app on one cluster.
- `-c/--cluster` exists only on `argocd app list`, as a filter, and takes a
  cluster name or server URL. It is applied client-side as an exact match on
  `.spec.destination.server` or `.spec.destination.name`, with no lookup
  between the two: `-c <url>` misses apps that target by name, and vice versa.
- So the cluster matters only for *finding* an app: pick a cluster, pick an
  app, then run app commands on its name.
- An Application's cluster is `.spec.destination.name` or
  `.spec.destination.server`. Group by that, nothing else: labels such as
  `argocd.cluster` are conventions of whoever writes the Applications, and
  can be missing or name the cluster differently. App names don't always
  contain their cluster's name either.
- `argocd app list` has no health/sync filter; filter its `-o json` on
  `.status.sync.status` / `.status.health.status` (see the fzf baseline).
- `argocd cluster list -o server` prints server URLs; `-o json` has names too.
- `argocd app list -o name` prints `<namespace>/<name>` (`argocd/<name>` for
  apps in the `argocd` namespace); app commands accept that form.
- A full `argocd app list` of about 600 apps (12 MB as `-o json`) takes
  ~0.6 s with `--grpc-web`, so a picker can list live, without a cache.
  `argocd app get` takes ~0.3 s, fast enough for a preview pane.
- `argocd app logs` and `argocd app actions run` go through the Argo API, so
  they reach any destination cluster without a kube context.

## Picking up a commit

- Argo CD polls git every two to three minutes by default
  (`timeout.reconciliation` 120 s plus up to 60 s of
  `timeout.reconciliation.jitter`, in `argocd-cm`; the webhook docs say
  "every three minutes"), so without a webhook it notices a commit minutes
  late, or later where the timeout is raised.
- `argocd app get <app> --refresh` is the fast path. A refresh you request
  (normal or hard) runs at comparison level `CompareWithLatestForceResolve`,
  which passes `noRevisionCache=true` to the repo-server, so the branch is
  re-resolved now. `--hard-refresh` also drops the rendered-manifest cache;
  it is for stale output at an unchanged commit. Given with `--refresh`,
  `--hard-refresh` wins.
- The refresh blocks: the API server's `Get` returns only after the controller
  has processed it. So this chain is safe, and is the "deploy it now, tell me
  when it's live" recipe for auto-synced apps:

  ```fish
  argocd app get <app> --refresh && argocd app wait <app> --sync --health --timeout 900
  ```

- `argocd app wait` checks current state: before Argo has seen the new commit
  the app is still Synced/Healthy on the old one, and `wait` returns at once.
  Refresh first.
- Apps that sync manually need `argocd app sync <app>` between the refresh
  and the wait, or the wait runs into its timeout.
- A fresh Deployment flips Progressing → Degraded once its
  `progressDeadlineSeconds` (600 by default) pass, while still Synced.
  Expected, not a failure.

## Restarting a workload

- `argocd app actions run <app> restart` works on Deployments, StatefulSets,
  DaemonSets and Argo Rollouts. Deployment's comes with Argo's built-in
  resource customizations (with pause, resume and scale); the other three
  were listed by `argocd app actions list`. Pick the workload with `--kind`,
  `--group`, `--namespace` and `--resource-name`, or take every match with
  `--all`.
- Which workloads are live takes `argocd app get`: `app list -o json` has no
  resource health at all. Argo CD 3 keeps resource health in the resource
  tree unless the application controller runs with `--persist-resource-health`
  (default false); its apps then say `status.resourceHealthSource: appTree`.
  The API server fills it in from the tree in `Get` (and Sync, Rollback,
  Watch), not in `List`.
- In `app get -o json`, a resource that is in git but not live has health
  `Missing` ("Resource has not been created"); kinds without a health check
  (ConfigMap, ServiceAccount, Namespace, RBAC) have no health. `actions list`
  on a workload that isn't live says "no matching resource found".
- Many apps have no restartable workload: CRD and ApplicationSet bundles,
  KEDA ScaledJobs, CronJobs.

## Retrying an aborted rollout

- An Argo Rollouts analysis that fails aborts its Rollout, whose health
  message then starts `RolloutAborted: Rollout aborted update to revision N:`
  and quotes the analysis. The failed AnalysisRun is never rerun; what gets
  retried is the Rollout (`kubectl argo rollouts retry` takes a rollout or an
  experiment).
- `argocd app actions run <app> retry --kind Rollout --resource-name <name>`
  is built in. It clears `status.abort` through the status subresource, as
  `kubectl argo rollouts retry rollout` sets `abort: false`, but through the
  Argo API and its RBAC (`action/argoproj.io/Rollout/retry`). It is enabled
  only while the rollout is aborted and not fully promoted.
- `argocd app actions list <app> --kind Rollout -o json` lists each action as
  `Group`, `Kind`, `Name`, `Action` and `Disabled`, without the namespace.
  With no Rollout live it fails: "no matching resource found".
- An aborted canary waits at step 0, so a retry runs every step again, with
  new AnalysisRuns made from the templates as they are then; a run whose name
  is taken gets a `.1`, `.2`, … suffix.
- A change to the Rollout's pod template or canary steps starts a new update
  and clears the abort itself, without a retry.
- `restart` is no retry. On a Rollout it sets `spec.restartAt` to now, and
  the controller evicts the pods created before then, `maxUnavailable` (at
  least one) at a time, stable ReplicaSet first: same revision, no steps, no
  analysis. Only a pod template or steps change, `promote-full`, a rollback,
  full promotion or the retry clear an abort.
- `argocd app sync --retry-limit` and the other `--retry-*` flags retry a
  failed sync, not an analysis.

## The preview pane

- `argocd app get -o json` has all the pane shows: sync status with the
  commit it resolved to, health, sync policy, source and destination, the
  last operation (`status.operationState`: phase, message, start and finish,
  who started it, the synced commit), `status.conditions`,
  `status.summary.images` and `.externalURLs`, and each resource's sync
  status, health and health message. The app itself has no health message.
- Condition types ending in `Error` are errors (`ComparisonError`,
  `InvalidSpecError`, `SyncError`, `DeletionError`, `UnknownError`); those
  ending in `Warning` are warnings (`SharedResourceWarning`,
  `OrphanedResourceWarning`, …). Shared-resource warnings can come by the
  dozen on one app.
- Argo CD's UI colors statuses green (Synced, Healthy, Succeeded), yellow
  (OutOfSync, Missing), red (Degraded, Failed, Error, Terminating), blue
  (Progressing, Running), purple (Suspended) and gray (Unknown).
- Its icons are Font Awesome. Health: heart (Healthy), heart-broken
  (Degraded), ghost (Missing), pause-circle (Suspended), spinning
  circle-notch (Progressing), question-circle (Unknown). Sync: check-circle
  (Synced), arrow-alt-circle-up (OutOfSync, or a trash can for a resource
  that requires pruning), spinning circle-notch (Unknown). Last operation:
  check-circle (Succeeded), times-circle (Failed, Error), spinning
  circle-notch otherwise.
- Nerd Fonts carry these as glyphs; the heart-crack (U+EF4F) and ghost
  (U+EEFE) arrived in Nerd Fonts 3.2.0.
- argocd's own text output has no colors. `app get` prints its resources as
  a tabwriter table (`GROUP KIND NAMESPACE NAME STATUS HEALTH HOOK MESSAGE`);
  `app wait`, `sync` and `rollback` print a row like it, with a timestamp,
  each time a resource changes. `app delete` asks before a cascading delete
  only when its stdout is a terminal.
- skim runs a preview command with `/bin/sh -c` in a pty the size of the
  pane, with `COLUMNS` and `ROWS` set; `COLUMNS` counts the pane's border
  column. Lines are cut at the pane's edge, not wrapped, unless the preview
  window has `wrap`. Shift-↑/↓ scroll the pane a line at a time, as does the
  mouse wheel over it; preview page keys exist but are unbound.

## Generated apps

- The ApplicationSet controller patches the apps it generates back to its
  template, so `app set` / `app patch` / `app edit` changes, including
  `--sync-policy none`, revert unless the ApplicationSet's
  `ignoreApplicationDifferences` covers the field. Change git or the
  ApplicationSet instead.
- `argocd app rollback` refuses while automated sync is on, and a generated
  app's auto-sync can't be durably turned off, so for those rollback means
  `git revert` in the gitops repo. `argocd app history` still shows what was
  deployed when. Apps that sync manually can use `argocd app rollback`.

## Login

- `argocd login <server> --sso`. The SSO callback is a local listener on port
  8085 (`--sso-port`); this works as-is on WSL2.
- `--grpc-web` is for proxies without HTTP/2. `argocd login` saves it (with
  `Core`, `PlainText`, `Insecure`) into the context in
  `~/.config/argocd/config`, so it is given once, at login.
- Behind such a proxy, without it, each `argocd` run first probes the server
  over native gRPC, fails, switches to gRPC-web and warns on stderr ("Failed
  to invoke grpc call. Use flag --grpc-web"). That costs ~0.1 s:
  `argocd version` took 0.25 s without the flag, 0.14 s with it.

## Commands worth knowing

```fish
argocd app list [-c <cluster>] [-p <project>] [-l <selector>] [-o name|json]
argocd app get <app> [-o tree=detailed] [--show-operation]    # status, resource tree, last sync + its error
argocd app diff <app>                                          # exit 0 none, 1 diff, 20 error, 2 bad args; Secrets left out
argocd app history <app>
argocd app manifests <app> --source live                       # default: --source git
argocd app logs <app> --kind Deployment --name <name> -f --tail 100
argocd app sync <app> [--preview-changes] [--resource GROUP:KIND:NAME]   # core group is blank: :ConfigMap:<name>
argocd app wait <app> --sync --health --timeout 900
argocd app actions run <app> restart --kind Deployment --resource-name <name>
argocd app actions run <app> retry --kind Rollout --resource-name <name>   # after a failed analysis
argocd app terminate-op <app>                                  # kill a stuck sync
argocd appset get <name>
argocd completion fish > ~/.config/fish/completions/argocd.fish
```

## fzf baseline

Run in fish on 2026-10-14, with `fzf --filter` standing in for the picker.

```fish
argocd app get (argocd app list -o name | fzf)              # any app, any cluster
argocd app list -c (argocd cluster list -o json | jq -r '.[].name' | fzf)    # pick a cluster, list its apps
argocd app list -o json | jq -r '.[] | select(.status.sync.status != "Synced" or .status.health.status != "Healthy") | [.metadata.name, .status.sync.status, .status.health.status] | @tsv'
```

On about 600 apps the third line printed over a quarter of them, about half
OutOfSync but Healthy: too many to serve as a "what's broken" view as-is.

## Ruled out

The plain CLI + fzf, or a small tool built in this repo, won over:

- **argonaut** (`darksworm/argonaut`; Go/Bubbletea, GPL-3; nixpkgs `argonaut`
  2.17.x): k9s-style TUI that drills clusters → namespaces → projects → apps,
  starts scoped via `default_view = "cluster <name>"`, and reuses
  `argocd login`. A UX reference.
- **k9s `plugins/argocd.yaml`**: shortcuts that `kubectl` annotate/patch
  Application objects on the Argo cluster (refresh = annotation, sync = patch
  `operation`). Acts with kube credentials instead of Argo SSO/RBAC; the
  Application view has no destination-cluster column; its disable-auto-sync
  reverts on generated apps.

## Decisions

Made 2026-10-14.

- **A Rust CLI called `aw`**, not fish functions, so it can be packaged with
  Nix. This repo's flake builds it with `buildRustPackage` and
  `cargoLock.lockFile`; another flake takes it as an input with
  `inputs.nixpkgs.follows` (see `README.md`).
- **It shells out to `argocd`**, so it keeps the SSO login, `--grpc-web` and
  contexts, and handles no tokens itself. The alternatives were the Argo CD
  HTTP API (token and SSO expiry handled by the tool) and the Kubernetes API
  on the Argo cluster (outside Argo SSO/RBAC).
- **The picker is skim, embedded**: crate `skim` 5.7 with
  `default-features = false`. It needs Rust 1.91 or newer (the nixpkgs this
  flake locks has 1.98.1). It draws on stderr and reads keys from
  `/dev/tty`, so `(aw …)` captures only the app name.
- **Commands pass through**: `aw <subcommand> [query] [-- flags]` finds the
  app and runs `argocd app <subcommand> <app> <flags>`. Bare `aw [query]`
  prints the app's name; `aw deploy` adds refresh, sync and wait.
- **Finding the app**: one `argocd app list`. An exact app name, or a query
  that only one app's name contains, skips the picker; otherwise skim opens
  with the query typed in. Every word between the command and `--` is part of
  the query.
- **State-changing commands ask first**: get, get-resource, diff, history,
  logs, manifests, resources, wait and refresh run straight away; everything
  else shows the command and cluster and asks y/N. deploy asks only before it
  syncs a manual-sync app. `AW_YES=1` turns asking off.
- **Short names**: `g` get, `d` diff, `s` sync, `l` logs, `h` history,
  `m` manifests, `w` wait, `r` refresh, `rs` restart, `dp` deploy. refresh,
  restart and deploy are `aw`'s own commands; the rest pass through.
- **restart** offers the app's live Deployments, StatefulSets, DaemonSets and
  Rollouts: one is used directly, several open a second picker (Tab marks
  more than one).

Made 2026-10-15.

- **The preview pane is `aw`'s own view** of `argocd app get -o json`: each
  picker row runs `aw --preview <app>`. It shows sync, health and policy,
  where the app deploys from and to, the last sync (with its message unless
  it succeeded), images and URLs, conditions (up to three messages of an
  error type, one of a warning type, the rest counted) and the resources,
  worst health first, then OutOfSync. It is laid out to the pane's width,
  wrapping long values with an indent. Statuses take the colors of Argo CD's
  UI, blue as cyan, in the pane and in the list's SYNC and HEALTH columns.
- **Statuses have Argo CD's icons**, as Nerd Font glyphs: before each
  status in the pane, and at the start of each list row, where they stay in
  view when the list is too narrow for its SYNC and HEALTH columns. Resource
  rows show only the two icons; the Resources line counts each status in
  words. Unknown sync gets a question mark like Unknown health, not the UI's
  spinner, which a still picture would show as progress. Fields have icons
  too. This needs a Nerd Font 3.2 or newer in the terminal.
- **get and refresh print the pane's view** to the terminal, unless given
  flags after `--`, which get `argocd app get`'s own output. The view is laid
  out to the width `crossterm::terminal::size()` reads from `/dev/tty`, so
  piped output keeps it (fish doesn't export `COLUMNS`), and has colors only
  when stdout is a terminal. Coloring argocd's own output instead was ruled
  out: icons would push its table columns out of line, and a filter on its
  stdout would drop `app delete`'s question.
- **restart asks `argocd app get`** which workloads are live: it takes the
  restartable kinds whose health isn't `Missing`.
- **It stays a CLI for now.** Floating it in herdr was discussed: a
  `type = "popup"` keybinding, or a plugin overlay pane. Either needs an
  app-first flow (pick the app, then the action) that keeps output on
  screen; not built.

Made 2026-10-16.

- **retry** (`rt`) offers the app's aborted Rollouts: the live ones in
  `argocd app get` whose `retry` action `argocd app actions list` shows
  enabled. That is Argo CD's own test, where matching the `RolloutAborted:`
  health message would lean on a message's wording. It skips `actions list`
  when no Rollout is live, as that fails then. Like restart, it asks first,
  takes a single Rollout directly and opens the second picker for several.

## Sources

- CLI reference: https://argo-cd.readthedocs.io/en/stable/user-guide/commands/argocd/
- Auto-sync and rollback: https://argo-cd.readthedocs.io/en/stable/user-guide/auto_sync/
- Git polling: https://argo-cd.readthedocs.io/en/stable/operator-manual/webhook/
  and the `timeout.reconciliation` comments in
  `docs/operator-manual/argocd-cm.yaml` (v3.5.3)
- ApplicationSet changes to generated apps: https://argo-cd.readthedocs.io/en/stable/operator-manual/applicationset/Controlling-Resource-Modification/
- Refresh force-resolve: `needRefreshAppStatus` and the `CompareAppState` call
  in https://github.com/argoproj/argo-cd/blob/master/controller/appcontroller.go
  (parameter names in `controller/state.go`)
- Refresh blocks: `func (s *Server) Get` in
  https://github.com/argoproj/argo-cd/blob/master/server/application/application.go
- Login saves `--grpc-web`: https://github.com/argoproj/argo-cd/blob/master/cmd/argocd/commands/login.go
- `app list -c` matching: `FilterByCluster` in
  https://github.com/argoproj/argo-cd/blob/master/util/argo/argo.go
- `app diff` exit codes:
  https://github.com/argoproj/argo-cd/blob/master/cmd/argocd/commands/app_diff.go
  (errors exit `ErrorGeneric` = 20 from `util/errors/errors.go`; the help
  text's "2 on general errors" holds only for a wrong argument count)
- `--grpc-web` probe: `NewClient` in
  https://github.com/argoproj/argo-cd/blob/v3.4.6/pkg/apiclient/apiclient.go
- `FilterByCluster` and the `app diff` exit codes read the same at v3.4.6
  (the CLI here), v3.5.3 and master; at v3.4.6 `app diff` is in `app.go`.
- Counts and timings: `argocd app list` / `app get` / `cluster list` /
  `version` against a v3.5.3 instance, 2026-10-14.
- Restart actions: `resource_customizations/apps/Deployment/actions/discovery.lua`
  in https://github.com/argoproj/argo-cd (v3.5.3); StatefulSet, DaemonSet and
  Rollout from `argocd app actions list`, 2026-10-14.
- Rollout retry: `discovery.lua` and `retry/action.lua` in
  `resource_customizations/argoproj.io/Rollout/actions/`, and
  `RunResourceActionV2` and `patchResource` in
  `server/application/application.go` (argo-cd v3.5.3);
  `pkg/kubectl-argo-rollouts/cmd/retry/retry.go`, `syncRolloutStatusCanary`
  in `rollout/canary.go`, `resetRolloutStatus` in `rollout/sync.go`,
  `PodTemplateOrStepsChanged` in `utils/replicaset/replicaset.go`,
  `newAnalysisRunFromRollout` in `rollout/analysis.go` and
  `CreateWithCollisionCounter` in `utils/analysis/helpers.go`, in
  https://github.com/argoproj/argo-rollouts (v1.8.3); the `--retry-*` flags
  from `argocd app sync --help` (v3.4.6).
- The `RolloutAborted:` health message: `GetRolloutPhase` in
  `utils/rollout/rolloututil.go` (argo-rollouts v1.8.3) writes
  `<reason>: <message>` to `status.message`, which
  `resource_customizations/argoproj.io/Rollout/health.lua` (argo-cd v3.5.3)
  passes through. `actions list`: `DisplayedAction` in
  `cmd/argocd/commands/app_actions.go` and `FilterResources` in
  `cmd/util/app.go` (argocd v3.4.6), and its `-o json` on a v3.5.3 instance,
  2026-10-16.
- Rollout restart: `resource_customizations/argoproj.io/Rollout/actions/restart/action.lua`
  (argo-cd v3.5.3); `Reconcile` in `rollout/restart.go` and the callers of
  `RemoveAbort` in `rollout/` (argo-rollouts v1.8.3).
- `--hard-refresh` beats `--refresh`: `getRefreshType` in
  `cmd/argocd/commands/app.go` (v3.4.6).
- Resource health in `app get` but not `app list`: `inferResourcesStatusHealth`
  and its callers in `server/application/application.go`; the
  `--persist-resource-health` flag in
  `cmd/argocd-application-controller/commands/argocd_application_controller.go`
  (both v3.5.3); `resourceHealthSource` and resource health from `app list`
  and `app get` on a v3.5.3 instance, 2026-10-15.
- Condition types: the `ApplicationCondition…` constants in
  `pkg/apis/application/v1alpha1/types.go` (v3.5.3).
- UI status colors: `ui/src/app/shared/components/colors.ts` (v3.5.3).
- UI status icons: `HealthStatusIcon`, `ComparisonStatusIcon` and
  `OperationPhaseIcon` in `ui/src/app/applications/components/utils.tsx`
  (v3.5.3).
- Nerd Font glyphs: `glyphnames.json` in https://github.com/ryanoasis/nerd-fonts
  (`fa-heart_crack` and `fa-ghost` absent at v3.1.1, present at v3.2.0).
- argocd's text output: `printAppResources`, `waitFormatString` and the
  callers of `waitOnApplicationStatus`, and the `isTerminal` check in
  `NewApplicationDeleteCommand`, in `cmd/argocd/commands/app.go` (v3.4.6).
- Terminal width: `size` in `src/terminal/sys/unix.rs` (crossterm 0.29.0)
  tries `/dev/tty`, then stdout, then `tput`. fish 4.9.3 keeps `COLUMNS`
  unexported (`set -S COLUMNS`, 2026-10-15).
- skim's preview pane: `run_preview` in `src/tui/app.rs`, `spawn` and
  `init_pty` in `src/tui/preview.rs`, the default keys in `src/binds.rs`
  (skim 5.7.4).
- argonaut: https://github.com/darksworm/argonaut and
  https://github.com/NixOS/nixpkgs/blob/master/pkgs/by-name/ar/argonaut/package.nix
- k9s plugin: https://github.com/derailed/k9s/blob/master/plugins/argocd.yaml
