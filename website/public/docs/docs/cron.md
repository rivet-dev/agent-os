# Crons & Loops

Schedule recurring commands and agent sessions in agentOS VMs.

Schedule recurring work and long-running agent loops with cron expressions,
running either a shell command (`exec`) or an agent session (`session`), with
overlap modes (`allow`, `skip`, `queue`) and native `cronEvent` streaming to
monitor execution. Cron jobs keep the actor alive while a job runs; the actor
can sleep between executions.

## Schedule a command

Run a shell command on a recurring schedule. Pass a custom `id` to make a job easier to manage and cancel later.

## Schedule an agent session

Create a recurring agent session that runs a prompt on a schedule.

## Overlap modes

Control what happens when a cron job triggers while a previous execution is still running.

| Mode | Behavior |
|------|----------|
| `"skip"` | Skip this trigger if the previous run is still active |
| `"allow"` | Allow concurrent executions (default) |
| `"queue"` | Queue this trigger and run it after the previous one finishes |

Prefer `"skip"` for most jobs to avoid unbounded concurrency if a run takes longer than the interval. Use `"queue"` when every trigger must eventually execute.

## Monitor cron events

Subscribe with `connection.on("cronEvent", ...)` to track job execution. Event and job timestamps are ISO strings. The callback receives a `CronEvent` directly, with no `{ event }` wrapper.

Subscribe before scheduling so you do not miss early runs.

## List and cancel cron jobs

## Example: Heartbeat pattern

Schedule a recurring agent session to periodically check on a task. This is the core pattern behind [OpenClaw](https://openclaw.org), where an agent wakes up on a schedule to review progress, take action, and go back to sleep.

The agent sleeps between executions and only consumes resources when the cron job fires.