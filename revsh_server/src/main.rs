use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use std::mem::size_of;
use std::sync::{Arc, Mutex};
use std::{io, io::BufRead};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::watch;

#[derive(Debug, Clone, Default, Serialize)]
enum Message {
    #[default]
    Default,
    Execute(String),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let listener = TcpListener::bind("0.0.0.0:6942").await?;

    println!("Listening on port 6942");

    let (shell_sender, shell_receiver) = watch::channel(Message::default());

    tokio::spawn(async move {
        loop {
            let (mut socket, addr) = listener.accept().await.unwrap();
            println!(
                "New client connected from {:?}:{:?}",
                addr.ip(),
                addr.port()
            );

            let mut shell_receiver = shell_receiver.clone();
            tokio::spawn(async move {
                loop {
                    shell_receiver
                        .changed()
                        .await
                        .expect("Error but shouldn't happen");
                    let u = shell_receiver.borrow().clone();

                    let srds = bincode::serialize(&u).unwrap();
                    socket.write_all(&srds.len().to_ne_bytes()).await.unwrap();
                    socket.write_all(&srds).await.unwrap();
                }
            });
        }
    });

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line.unwrap();
        shell_sender.send(Message::Execute(line.into())).unwrap();
    }

    Ok(())
}
