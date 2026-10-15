# argo-cli

Terminal tooling for an Argo CD instance that deploys to many clusters: find
an app and act on it without retyping `argocd app list -c <cluster>`.

The tool is `aw`, a Rust CLI that shells out to `argocd`; `README.md` is its
usage. Build and test with `nix develop -c cargo test` and `nix build` (a flake
only sees files git tracks; `nix build path:.` works before they are
committed). `tests/cli.rs` runs `aw` against a fake `argocd`, which is how the
state-changing commands (sync, refresh, restart, deploy) are tested.

Read `FINDINGS.md` before designing or changing anything: it holds the
verified `argocd` behaviour this tool wraps, what has been ruled out, and the
decisions made. `FINDINGS.local.md`, when present, adds what was found about
the instance in use. Argo behaviour in neither is unverified until checked
against the Argo CD docs or source; record what you verify there.

This repo is public. Anything about a particular instance or company (its
hostname, cluster, app, project and repo names, its counts) goes in the
gitignored `FINDINGS.local.md`, never in tracked files; tests and docs use
made-up names.
