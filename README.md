# aw

Find an Argo CD app and act on it. `aw` wraps the `argocd` CLI: it finds the
app, from a query or with a fuzzy picker, then runs the `argocd app` command
on it. Login, SSO, `--grpc-web` and contexts stay `argocd`'s.

## Install

`aw` runs the `argocd` on your PATH, which must be logged in
(`argocd login <server> --sso`). Its icons are Nerd Font glyphs: the terminal
needs a Nerd Font, 3.2 or newer, or they show as boxes.

```sh
nix build          # result/bin/aw
nix run . -- g     # or run it from the checkout
```

From another flake:

```nix
inputs.awrap = {
  url = "github:antonlundstroem/awrap";
  inputs.nixpkgs.follows = "nixpkgs";
};
# the package: inputs.awrap.packages.${system}.default
```

## Usage

```
aw [query]                         pick an app, print its name
aw <command> [query] [-- flags]    pick an app, run the command on it
```

| short | command   | runs                                                  |
|-------|-----------|-------------------------------------------------------|
| `g`   | get       | `argocd app get <app>`, shown like the picker's pane  |
| `d`   | diff      | `argocd app diff <app>`                               |
| `s`   | sync      | `argocd app sync <app>`, after asking                 |
| `l`   | logs      | `argocd app logs <app>`                               |
| `h`   | history   | `argocd app history <app>`                            |
| `m`   | manifests | `argocd app manifests <app>`                          |
| `w`   | wait      | `argocd app wait <app>`                               |
| `r`   | refresh   | `argocd app get <app> --refresh`, shown the same way  |
| `rs`  | restart   | `argocd app actions run <app> restart …`, after asking |
| `dp`  | deploy    | refresh, sync if needed, wait                         |

Every other `argocd app` subcommand that takes an app name works under its
full name: `aw rollback`, `aw terminate-op`, `aw get-resource` and so on.
`create`, `list` and `actions` don't; for those, put the bare form where the
app name goes, e.g. `argocd app actions run (aw billing) …` in fish
(`$(aw billing)` in bash).

Flags after `--` go to `argocd`: `aw l billing -- -f --tail 100`,
`aw r billing -- --hard-refresh`. deploy and restart take none.

### Finding the app

`aw` lists every app once (`argocd app list`). If the query is an app's exact
name, or appears in exactly one app's name, that app is used. Otherwise a
picker opens with the query typed in. Each row is the app's sync and health
icons, then `app cluster sync health`. Words separated by spaces must all
match (`prod degraded`); `|` means or. Esc picks nothing.

The pane on the right shows the highlighted app from `argocd app get`: sync
and health, where it deploys from and to, the last sync and why it failed,
images, URLs, conditions, and the resources, worst first. Statuses have the
icons and colors of Argo CD's UI: a green check for Synced and heart for
Healthy, a yellow arrow for OutOfSync and ghost for Missing, a red broken
heart for Degraded, a cyan spinner for Progressing, a magenta pause for
Suspended, a trash can for a resource a sync would prune. Shift-↑/↓ or the
mouse wheel scroll the pane.

All the words between the command and `--` form the query:
`aw d jobs import` searches for `jobs import`.

### Asking first

Commands that only read run straight away: get, get-resource, diff, history,
logs, manifests, resources, wait and refresh. Everything else shows the
argocd command and the app's cluster, then asks `[y/N]`; anything but `y`
runs nothing. Set `AW_YES=1` to stop asking (`set -Ux AW_YES 1` in fish).

### get and refresh

They print the app the way the picker's pane shows it, laid out to the
terminal's width, in color when printing to a terminal. With flags after `--`
they print argocd's own output instead: `aw g billing -- -o yaml`.

### deploy

1. `argocd app get <app> --refresh`: Argo re-reads git and compares now
   instead of at its next poll.
2. If the app is out of sync and syncs manually: ask, then
   `argocd app sync <app>`. Auto-sync apps sync themselves.
3. `argocd app wait <app> --sync --health --timeout 900`.

### restart

Restarts the app's live Deployments, StatefulSets, DaemonSets and Argo
Rollouts. With one, it is used directly; with several, a second picker opens,
where Tab marks more than one.

## Exit codes

The argocd command's own when `aw` ran one. 130 when you leave a picker with
Esc; 1 when you answer no, there is nothing to restart, or argocd can't run;
2 for a bad `aw` command line.
