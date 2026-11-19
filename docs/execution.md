# JSON execution tasks

Use one command for builds, device operations and observations:

```bash
gizmoth request -f request.json
```

The executable shown in the installed GizMoth Skill is absolute; use it when `gizmoth` is not on PATH. `-f -` reads a JSON document from stdin. The command connects to GizMoth's local resident service and starts it if needed. Clients do not need to implement a WebSocket, hold credentials or keep a CLI process alive to retain an accepted task.

`--data-dir` is optional. The CLI resolves the installed app's default data directory automatically; specify another directory only to address a separate instance. `request` replaces the previous `exec` command name; update existing shell commands to use `request`. JSON envelopes, persisted history and result formats are unchanged.

To list managed serial ports and devices directly from stdin:

```bash
printf '%s\n' '{"apiVersion":"gizmoth.dev/v1","kind":"Request","spec":{"operation":"serial.list"}}' | gizmoth request -f -
```

Use `serial.ports` to discover system serial interfaces, and `serial.status` with a returned port ID to inspect a device. These observations return JSON without acquiring a device grant or creating history. Device identity is cached; inspect `connection`, `present` and `h2loaderAt`, and use `serial.refresh` when fresh H2Loader status is needed. USB adapter metadata does not establish board identity.

## Request document

```json
{
  "apiVersion": "gizmoth.dev/v1",
  "kind": "Request",
  "metadata": { "name": "Build firmware" },
  "spec": {
    "operation": "compile",
    "workspace": "/absolute/host/project",
    "command": ["bazel", "build", "//app:firmware"],
    "artifacts": ["bazel-bin/app/firmware.bin"]
  }
}
```

`metadata` and its `name` are optional. `spec.operation` selects the operation; its parameters live beside `operation`. Requests are limited to 64 KiB. Unsupported versions, kinds, operations and parameters are rejected.

Discover the current operation schemas with this safe request:

```json
{
  "apiVersion": "gizmoth.dev/v1",
  "kind": "Request",
  "spec": { "operation": "describe" }
}
```

The response contains `result.requestSchema` and `result.operations[].inputSchema`. Consult these schemas before constructing device requests.

| Operation | Parameters | History |
| --- | --- | --- |
| `describe` | None | Observation only |
| `serial.list` | None; returns `result.ports` with all managed ports, connection state and cached H2Loader device details | Observation only |
| `serial.ports` | None; returns `result.ports` with system-discovered serial paths and USB metadata, including unmanaged ports | Observation only |
| `serial.status` | Exact managed port ID in `port`; returns the full current port view, including `h2loader` and `h2loaderAt` | Observation only |
| `compile` | `workspace`, `command`, optional `workspaceMode` (`sync` or `direct`), `cwd`, `env`, `artifacts`, `exclude` (sync mode), `timeoutSeconds`, `collectOnFailure` | Creates an action job |
| `serial.send` | `port`, `text`, optional `lineEnding` | Creates an action job |
| `serial.install` | `port`, an absolute `package` path or `artifact` ID, optional `file`, `stageOnly`, `jobId` | Creates an action job |
| `serial.refresh` | `port`, optional `jobId` | Creates an action job |
| `serial.reboot` | `port`, `target`, optional `jobId` | Creates an action job |
| `jobs.list` | None | Observation only |
| `jobs.get` | `id`, optional `waitSeconds` (default 0) | Observation only |
| `jobs.wait` | `id`, optional `waitSeconds` (default 20) | Observation only |
| `jobs.cancel` | `id` | Creates a cancellation action job |
| `logs.read` | `source` (`compile` or `serial`), `id`, optional `cursor`, `limit`, `waitSeconds` | Observation only |

For a project with `main.c`, save the following as `direct.json` and run `gizmoth request -f direct.json`. Set `workspace` to the absolute project path. The compiler reads `main.c` directly from the read-only `/src` mount. The executable appears at `<workspace>/out/app` and the artifact store retains a snapshot for this build.

```json
{"apiVersion":"gizmoth.dev/v1","kind":"Request","spec":{"operation":"compile","workspaceMode":"direct","workspace":"/absolute/host/project","command":["cc","main.c","-o","/out/app"],"artifacts":["app"]}}
```

GizMoth's managed Linux slots provide shared Bazel/ccache caches and artifact collection in both modes. See [direct build paths and retention](compiling.md#direct-workspace-builds).

Device actions use the same FIFO ownership queue, approval controls and job fencing as existing device sessions. The resident service acquires and retains the grant until the action settles. This version executes device actions on local managed ports; imported remote ports still require the existing remote session transport. Device operations require authorization for the actual target and package. GizMoth remains the process that opens the physical serial port and runs H2Loader.

## Waiting, detaching and output

Without `--detach`, the CLI waits until its action reaches a terminal state. Without `--output`, stdout contains one final JSON response. A successful action returns `{ "job": {...}, "result": ... }`; failures include the job's terminal status and error. Observations return `{ "result": ... }`.

```bash
gizmoth request -f request.json --detach
```

Detached execution returns an immediate `{ "job": {...} }` receipt. The service owns the task after the CLI exits. Save `job.id` to observe or cancel it later. Disconnecting this CLI does not cancel it; closing a legacy build WebSocket retains that transport's cancellation behavior.

```bash
gizmoth request -f request.json --output /absolute/new-result.json
```

`--output` selects a new JSON result file. The CLI resolves a relative path against its current directory. Its parent must exist, and an existing file is never overwritten, including one created while the task runs. With `--output`, stdout omits the final result body: actions return `{ "job": {...} }` including the output path, and observations return `{ "output": "/absolute/new-result.json" }`. With `--detach --output`, stdout still returns the immediate job receipt; the service writes the file when execution finishes. A publication failure marks the job failed and records the error.

Exit codes are 0 for successful observations, successful actions or accepted detached jobs; 1 for terminal failure, cancellation or interruption; and 2 for argument, request or transport errors. Acceptance is not proof that a detached task succeeded. An observation's successful exit does not establish that a job nested in its result succeeded; inspect that job's status.

## Observe or cancel through JSON

Use the same command with another request document:

```json
{
  "apiVersion": "gizmoth.dev/v1",
  "kind": "Request",
  "spec": { "operation": "jobs.wait", "id": "RETURNED_JOB_ID", "waitSeconds": 20 }
}
```

`jobs.get` and `jobs.wait` return a receipt inside `result`; inspect `result.job.status`. Waits are bounded at 120 seconds and may return a job that is still running. Repeat the observation as needed. Terminal states are `succeeded`, `failed`, `cancelled` and `interrupted`; other states are `queued`, `running` and `awaiting_approval`.

To cancel, submit `spec: { "operation": "jobs.cancel", "id": "RETURNED_JOB_ID" }`. This creates its own cancellation action. Cancellation requests stop the target at its execution boundary; observe the target until it settles.

`logs.read` reads the existing log subsystem on demand. For compile logs, `id` can be the executor job ID or underlying compile job ID. For serial logs, use the managed port ID. Pass the returned `next` cursor to subsequent calls. Reading logs does not create an execution-history entry.

## Execution tasks page and persistence

**Execution tasks** in the sidebar accepts a JSON request and an optional new output-file path. Action history is shared with the CLI. Expand a row to inspect or copy its request JSON, status, timing and error, or reveal its recorded output file.

History persists request documents and job metadata under `<app data>/execution/`. It does not persist streaming logs or stdout result bodies. Existing compile and serial log retention is independent of this history. In-memory results are available while the service retains them; select `--output` when a result must survive restart. A restarted service marks unfinished history entries `interrupted` and does not replay them.

The local executor routes require the desktop's authenticated private-loopback authority. Gateway API keys do not grant access to local execution, history or output-file paths. Existing CLI read-only tools, MCP, device WebSocket and compile WebSocket APIs remain available for compatibility; new Skills should prefer `gizmoth request` for execution.
