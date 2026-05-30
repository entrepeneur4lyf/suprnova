# Scheduling Commands

suprnova provides CLI commands for running and managing scheduled tasks.

## schedule:run

Run all due scheduled tasks once. This is typically called by cron every minute.

```bash
suprnova schedule:run
```

### Example Output

```
-> Running due scheduled tasks...

  [2024-01-15 03:00:01] Running 'cleanup:logs'... OK
  [2024-01-15 03:00:01] Running 'send:reminders'... OK

Done.
```

### Use Case

Add to your crontab to run every minute:

```bash
* * * * * cd /path/to/project && suprnova schedule:run >> /dev/null 2>&1
```

---

## schedule:work

Start the scheduler daemon that continuously checks for due tasks every minute.

```bash
suprnova schedule:work
```

### Example Output

```
-> Starting scheduler daemon...
Press Ctrl+C to stop

==============================================
  suprnova Scheduler Daemon
==============================================

  Registered tasks: 3
  Press Ctrl+C to stop

==============================================

[2024-01-15 03:00:00] Running 'cleanup:logs'
[2024-01-15 03:00:00] Task 'cleanup:logs' completed
[2024-01-15 09:00:00] Running 'send:reminders'
[2024-01-15 09:00:00] Task 'send:reminders' completed
```

### Use Case

Ideal for:
- **Development**: Easy testing without setting up cron
- **Docker**: Run as your container's scheduler process
- **Systemd**: Managed as a background service

---

## schedule:list

Display all registered scheduled tasks with their schedules.

```bash
suprnova schedule:list
```

### Example Output

```
Scheduled Tasks:
==========================================================================================
Name                           Schedule                       Description
------------------------------------------------------------------------------------------
cleanup:logs                   0 3 * * *                      Removes logs older than 30 days
send:reminders                 0 9 * * *                      Sends daily reminder emails
backup:database                0 0 * * 0                      Weekly database backup
heartbeat                      * * * * *                      Health check ping
==========================================================================================
Total: 4 task(s)
```

---

## Running Specific Tasks

You can run a specific task by name using the schedule binary directly:

```bash
cargo run --bin schedule -- run-task cleanup:logs
```

### Example Output

```
Task 'cleanup:logs' completed successfully.
```

If the task doesn't exist:

```
Task 'unknown-task' not found.

Available tasks:
  - cleanup:logs
  - send:reminders
  - backup:database
```

---

## Prerequisites

Before using schedule commands, you need:

1. **At least one task**: Create with `suprnova make:task TaskName`
2. **schedule.rs**: Created automatically by `make:task`
3. **bin/schedule.rs**: Created automatically by `make:task`

If these don't exist, you'll see:

```
Error: No schedule.rs found at src/schedule.rs
Run 'suprnova make:task <name>' to create your first scheduled task.
```

---

## Summary

| Command | Description |
|---------|-------------|
| `suprnova schedule:run` | Run all due tasks once (for cron) |
| `suprnova schedule:work` | Run as daemon (continuous) |
| `suprnova schedule:list` | List all registered tasks |
| `cargo run --bin schedule -- run-task <name>` | Run specific task |
