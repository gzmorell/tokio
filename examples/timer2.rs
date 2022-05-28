use tokio::time;

async fn task_that_takes_a_second() {
    println!("hello");
    time::sleep(time::Duration::from_secs(1)).await
}

#[tokio::main]
async fn main() {
    let mut interval = time::interval(time::Duration::from_secs(2));
    let mut count = 0;
    loop {
        tokio::select! {
            _ = interval.tick() => {
                count += 1;
                if count >= 10 {
                    break
                } else {
                    task_that_takes_a_second().await
                }
             }
        }
    }
}
