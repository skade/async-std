# Performance tooling for async-std

## How to enable

Depend on `async-std` with _both_ the `unstable` and the `tracing` flag on.

This can be used on any application using `async-std`.

## Available probes

Several probes are available.

* `async_std:task_spawn` will trigger if a new task is spawned, listing:
    * `task_id`: The spawned tasks `id`
    * `parent_id`: The spawning tasks (parent) `id`
    * `name`: The name of the task (currently defunct)
* `async_std:task_completed` will trigger if a task is completed, listing:
    * `task_id`: The completed tasks id
* `async_std:machine_scheduled_task` will trigger if a machine schedules a new task, listing:
    * `machine_addr`: The memory address of the scheduling machine instance
    * `task_id`: the `id` of the task being scheduled
    * `local`: whether or not the task was scheduled _locally_ or globally
* `async_std:machine_blocked` will trigger if a machine becomes blocked by exhausting its runtime limit:
    * `machine_addr`: The memory address of the blocked machine instance
* `async_std:machine_starting`: will trigger if a new machine is being started:
    * `machine_addr`: The memory address of the blocked machine instance

## Debug symbols

Debug symbols need to be available to install probes. You can compile using debug symbols using:

```
[profile.release]
debug = true
```

in your `Cargo.toml`.

## Installing probes

Probes need to be installed _per application_ and can be installed one by one. To install, use:

```sh
sudo perf probe -x target/release/my_app --add "async_std:task_spawn=async_std_task_spawn:0 task_id parent_id name" 
```

See `setup_probes.sh` for a script that does the setup for you.

Note that if a code path containing a probe is fully eliminated during optimisation, the probe will not install (which isn't bad, it would also never trigger).

Probes may need to be reinstalled if you are tracing a new binary.

## Deleting probes

It is recommended to delete all probes after using them, do so using:

```sh
sudo perf probe -d "async_std:*"
```

## Recording

To record a run, use:

```sh
sudo perf record target/releae/my_app -e "async_std:*"
```

See `man perf-record` for more options, such as attaching to a running process.

This will write a `perf.data` file.

## Aggregating results

You can use `perf script` to rerun a trace and use a script to aggregate the results.

See `aggregate-machine-and-task-stats.py` for such a script as an example. See `man perf-script-python` for details.

```sh
perf script -s perf/aggregate-machine-and-task-stats.py
```

Note that `perf.data` might be owned by root and ownership needs to be changed before running the script.
