#[no_mangle]
#[cfg_attr(feature = "tracing", inline(never))]
#[cfg_attr(not(feature = "tracing"), inline(always))]
#[export_name="async_std_machine_starting"]
/// Called whenever a machine is started.
pub fn machine_starting(machine_addr: *const ()) {
}

#[no_mangle]
#[cfg_attr(feature = "tracing", inline(never))]
#[cfg_attr(not(feature = "tracing"), inline(always))]
#[export_name="async_std_machine_scheduled_task"]
/// Called whenever a machine scheduled a task, locally or in the global runtime.
pub fn machine_scheduled_task(machine_addr: *const (), task_id: u64, local: bool) {
}


#[no_mangle]
#[cfg_attr(feature = "tracing", inline(never))]
#[cfg_attr(not(feature = "tracing"), inline(always))]
#[export_name="async_std_machine_blocked"]
/// Called whenever a machine is blocked an its processor is being
/// stolen.
pub fn machine_blocked(machine_addr: *const ()) {
}

