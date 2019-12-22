use async_std::task;
use std::thread;
use std::time::Duration;

fn main() {
     task::block_on(async {
         let mut tasks: Vec<task::JoinHandle<()>> = vec![];
         let task = task::spawn(async {
             task::sleep(Duration::from_millis(1000)).await;
         });
         let blocking = task::spawn_blocking(|| {
             thread::sleep(Duration::from_millis(1000));
         });
         tasks.push(task);
         tasks.push(blocking);
         for task in tasks {
             task.await
         }
     });
}

