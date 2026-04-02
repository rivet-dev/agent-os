> **Preview** This package is in preview and may have breaking changes.

# @rivet-dev/agent-os-google-drive

Declarative Google Drive native mount helper for Agent OS. This package keeps
the public helper surface on the TypeScript side while routing first-party
Google Drive-backed filesystems through the native `google_drive` sidecar
plugin.

## Usage

```ts
import { AgentOs } from "@rivet-dev/agent-os";
import { createGoogleDriveBackend } from "@rivet-dev/agent-os-google-drive";

const vm = await AgentOs.create({
  mounts: [
    {
      path: "/data",
      plugin: createGoogleDriveBackend({
        credentials: {
          clientEmail: "...",
          privateKey: "...",
        },
        folderId: "your-google-drive-folder-id",
      }),
    },
  ],
});
```

## Configuration

| Option | Type | Required | Description |
|--------|------|----------|-------------|
| `credentials` | `{ clientEmail: string; privateKey: string }` | Yes | Google service account credentials |
| `folderId` | `string` | Yes | Google Drive folder ID where blocks are stored |
| `keyPrefix` | `string` | No | Optional prefix for the persisted manifest and block file names |
| `chunkSize` | `number` | No | Optional persisted block chunk size used by the native plugin |
| `inlineThreshold` | `number` | No | Optional maximum inline file size stored in the manifest before chunking |

## Rate Limits

Google Drive API has a rate limit of approximately 10 queries/sec/user. Heavy
I/O workloads may experience throttling. Consider larger `chunkSize` values for
write-heavy workloads so the native plugin emits fewer Drive API calls.
