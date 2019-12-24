cargo build --example task-migration --features unstable,tracing --release

sudo perf probe -x target/release/examples/task-migration \
--add "async_std_task_spawn:0 task_id parent_id name" \
--add "async_std_task_completed:0 task_id" \
--add "async_std_machine_scheduled_task:0 machine_addr task_id local" \
--add "async_std_machine_blocked:0 machine_addr"
# --add "async_std_machine_starting:0 machine_addr" \ #-- for some reason, async_std_machine_starting has no debug-info generated in release mode...

#sudo perf trace -e "probe_task:*" --output trace.file target/release/examples/task-migration
#sudo perf record -e "probe_task:*" target/release/examples/task-migration
#
#sudo perf probe -d "async_std*"
