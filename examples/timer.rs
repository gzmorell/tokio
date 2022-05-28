use tokio::time;
use std::time::{Instant};

async fn task_that_takes_a_second() {
    println!("hello");
    time::sleep(time::Duration::from_secs(1)).await
}

#[tokio::main]
async fn main() {
    let mut interval = time::interval(time::Duration::from_secs(2));
    let start = Instant::now();

    for _i in 0..5 {
        interval.tick().await;
        let duration = start.elapsed();
        println!("Time elapsed is {:?}", duration);
        task_that_takes_a_second().await;
    }
}
