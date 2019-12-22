use async_std::sync::channel;
use async_std::task;

fn main() {
    let (sender1, receiver1) = channel::<String>(10);
    let (sender2, receiver2) = (sender1.clone(), receiver1.clone());

    task::spawn(async move {
        loop {
            if let Some(msg) = receiver1.recv().await {
                println!("Receiver 1 received: {}", msg);
            }
        }
    });
    task::spawn(async move {
        loop {
            if let Some(msg) = receiver2.recv().await {
                println!("Receiver 2 received: {}", msg);
            }
        }
    });
    task::spawn(async move {
        loop {
            sender1.send("message from Sender 1".into()).await;
        }
    });
    task::spawn(async move {
        loop {
            sender2.send("message from Sender 2".into()).await;
        }
    });

    task::block_on(async { loop { } });
}
