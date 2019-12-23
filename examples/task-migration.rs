use async_std::task;
use rand::{thread_rng,Rng};

fn main() {
    task::block_on(async {
        let mut tasks = Vec::new();

        let mut rng = thread_rng();

        for i in 1..100 {
            let micros = rng.gen_range(50, 5000);

            let task = task::spawn(async move {
                std::thread::sleep(std::time::Duration::from_micros(micros));
                println!("Done sleeping: {}, micros: {}", i, micros);
            });
            tasks.push(task);
        }

        for task in tasks {
            task.await
        }
    });
}
