use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use std::mem::size_of;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::{io, io::BufRead};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket};
use tokio::process;
use tokio::sync::watch;

#[derive(Debug, Clone, Default, Deserialize)]
enum Message {
    #[default]
    Default,
    Execute(String),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let bjr = std::env::args().skip(1).next().expect("Missing argument");

    let socket = TcpSocket::new_v4().unwrap();
    let address: SocketAddr = bjr.parse().unwrap();
    let mut stream = socket.connect(address.clone()).await?;

    println!("Successfully connected to {:?}!", address);

    loop {
        let mut len_buf = [0u8; size_of::<usize>()];
        stream.read_exact(&mut len_buf).await.unwrap();
        let len = usize::from_ne_bytes(len_buf);
        let mut buffer = vec![0u8; len];
        stream.read_exact(&mut buffer).await.unwrap();

        let mess: Message = bincode::deserialize(&buffer).unwrap();
        match mess {
            Message::Execute(d) => {
                let child = process::Command::new("sh")
                    .arg("-c")
                    .arg(d)
                    .spawn()
                    .expect("failed to spawn");
            }
            _ => (),
        }
    }
}
