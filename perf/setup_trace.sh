#!/bin/sh

# some of these probes might not register if the code path triggering them is _entirely_ removed
sudo perf probe -x $1 --add "async_std:task_spawn=async_std_task_spawn:0 task_id parent_id name" 
sudo perf probe -x $1 --add "async_std:task_completed=async_std_task_completed:0 task_id"
sudo perf probe -x $1 --add "async_std:machine_scheduled_task=async_std_machine_scheduled_task:0 machine_addr task_id local"
sudo perf probe -x $1 --add "async_std:machine_blocked=async_std_machine_blocked:0 machine_addr"
sudo perf probe -x $1 --add "async_std:machine_starting=async_std_machine_starting:0 machine_addr"

# to delete the probes, use `sudo perf probe -d "async_std:*"`
