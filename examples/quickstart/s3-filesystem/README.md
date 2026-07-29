---
title: "S3 Filesystem"
description: "Mount an S3-compatible bucket via createS3Backend and use it as a local filesystem."
category: "Quickstart"
order: 10
---

Reach for this when you want a VM to read and write files that live in an S3-compatible bucket (AWS S3, MinIO, or any signed-request service) instead of ephemeral local storage. The guest code uses plain filesystem calls — the bucket is just a mount.

## How it works

A `chunked_s3` mount descriptor (built into `@rivet-dev/agentos`) configures an S3 plugin from a bucket, prefix, region, and credentials. Passing it as a mount at `/mnt/data` makes the VM treat the bucket as a normal directory, so `writeFile`, `readFile`, and `readdir` operate transparently against S3. Configure the real bucket through the `S3_*` environment variables; when they are absent the example boots a strict local S3 harness so the same flow still runs against signed requests.

## Run it

```sh
pnpm install
# Optional: export S3_BUCKET, S3_REGION, S3_ACCESS_KEY_ID, S3_SECRET_ACCESS_KEY
#           (and S3_ENDPOINT for MinIO). Omit them to use the local harness.
pnpm --dir examples/quickstart/s3-filesystem start
```

Writes `/mnt/data/notes.txt` to the bucket, reads it back, and prints the directory listing.

## Source

View the source on GitHub: https://github.com/rivet-dev/agent-os/tree/main/examples/quickstart/s3-filesystem
