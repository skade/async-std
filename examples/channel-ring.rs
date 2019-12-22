///Ring benchmark inspired by Programming Erlang: Software for a
///Concurrent World, by Joe Armstrong, Chapter 8.11.2
///
///"Write a ring benchmark. Create N processes in a ring. Send a
///message round the ring M times so that a total of N * M messages
///get sent. Time how long this takes for different values of N and M."

use async_std::sync::channel;
use async_std::task;
use async_std::prelude::*;
use std::time::SystemTime;

#[derive(Debug)]
struct Payload(u64);
struct End;

fn print_usage_and_exit() -> ! {
    eprintln!("Usage; channel-ring <num-nodes> <num-times-message-around-ring>");
    ::std::process::exit(1);
}

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() < 3 {
        print_usage_and_exit();
    }
    let n = if let Ok(arg_num_nodes) = args[1].parse::<u64>() {
        if arg_num_nodes <= 1 {
            eprintln!("Number of nodes must be > 1");
            ::std::process::exit(1);
        }
        arg_num_nodes
    } else {
        print_usage_and_exit();
    };

    let m = if let Ok(arg_ntimes) = args[2].parse::<u64>() {
        arg_ntimes
    } else {
        print_usage_and_exit()
    };

    let limit = n * m;

    let setup_time = SystemTime::now();
    let (mut senders, mut receivers) = (Vec::new(), Vec::new());

    for _ in 0..n {
        let (sender, receiver) = channel::<Payload>(1);
        senders.push(sender);
        receivers.push(receiver);
    }

    let mut senders = senders.into_iter();
    let mut receivers = receivers.into_iter();

    let first_sender = senders.next().unwrap();
    let (buz_send, mut buz_recv) = channel::<End>(1);

    while let Some(mut receiver) = receivers.next() {
        if let Some(sender) = senders.next() {
            let node = async move {
                while let Some(mut p) = receiver.next().await {
                    p.0 += 1;
                    sender.send(p).await;
                }
            };
            task::spawn(node);
        } else {
            let sender = first_sender.clone();
            let buz_send = buz_send.clone();

            let node = async move {
                while let Some(mut p) = receiver.next().await {
                    p.0 += 1;

                    if p.0 >= limit {
                        println!("Reached limit: {}, payload: {}", limit, p.0);
                        break;
                    }
                    sender.send(p).await;
                }
                buz_send.send(End).await;
            };
            task::spawn(node);
        }
    }

    let elapsed = setup_time.elapsed().unwrap();

    println!("Time taken for setup: {}.{:06} seconds",
        elapsed.as_secs(),
        elapsed.subsec_micros());

    task::block_on(async move { 
        let now = SystemTime::now();
        first_sender.send(Payload(0)).await;
        buz_recv.next().await;
        let elapsed = now.elapsed().unwrap();

        println!("Time taken: {}.{:06} seconds",
            elapsed.as_secs(),
            elapsed.subsec_micros());
    });
}
