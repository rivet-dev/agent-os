---
title: "Git"
description: "Clone a local git repository into the VM from its feature-branch HEAD."
category: "Quickstart"
order: 8
---

Run real `git` inside a VM: initialize a repo, branch, commit, and clone it — observing that the clone inherits whichever branch was HEAD at clone time. Reach for this when your agent needs version control or to materialize a repository from a feature branch.

## How it works

The VM is created with the `@agentos-software/git` package mounted as software and filesystem, child-process, and env permissions enabled. From there it drives ordinary git commands through `vm.process.exec`, mixing in `vm.filesystem.writeFile`/`vm.filesystem.readFile` to stage files and inspect results. It commits on a `feature` branch so that `git clone` resolves HEAD to `feature`, then reads the clone's `.git/HEAD` and the feature file to confirm the branch carried over.

## Run it

```bash
npm install
npx tsx index.ts
```

Prints the origin's default branch, the clone's HEAD (`feature`), and the contents of the cloned `feature.txt`.

## Source

View the source on GitHub: https://github.com/rivet-dev/agent-os/tree/main/examples/quickstart/git
