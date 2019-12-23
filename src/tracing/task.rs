#[no_mangle]
#[cfg_attr(feature = "tracing", inline(never))]
#[cfg_attr(not(feature = "tracing"), inline(always))]
#[export_name="async_std_task_spawn"]
/// Called whenever a task is spawned.
///
/// Both the task id and its parents id will be passed, along with the name,
/// if given.
/// 
/// The id may be reused, but it is guarenteed that between `spawn` and
/// `drop`, the task remains the same.
pub fn spawn(task_id: u64, parent_id: u64, name: Option<&str>) {

}

#[no_mangle]
#[cfg_attr(feature = "tracing", inline(never))]
#[cfg_attr(not(feature = "tracing"), inline(always))]
#[export_name="async_std_task_completed"]
/// Called whenever a task is completed. The id is the id of the current task,
/// the name will be passed, if given.
pub fn completed(task_id: u64) {

}
