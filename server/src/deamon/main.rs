use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use std::mem::size_of;
use std::sync::{ Arc, Mutex, RwLock };
use std::io::{ self, BufRead, ErrorKind };
use std::net::SocketAddr;
use std::collections::HashMap;
use tokio::io::{ AsyncReadExt, AsyncWriteExt };
use tokio::fs as afs;
use tokio::net::{ TcpListener, UnixListener, UnixStream };
use tokio::sync::{ mpsc, broadcast };
use chrono::{ DateTime, Utc };

use revsh_common::*;
use revsh_server::*;

struct Client {
    pub uid: UID,
    pub connected_since: DateTime<Utc>,
    pub addr: SocketAddr,

    pub out_events: mpsc::Sender<OutClientEvent>,
}

#[derive(Debug, Clone)]
enum GlobalEvent {
    NewClient {
        uid: UID,
    },
    ClientDisconnect {
        uid: UID,
    },
    ClientMessage {
        sender: UID,
        message: C2SMessage,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let listener = TcpListener::bind("0.0.0.0:6942").await?;
    println!("Listening on port 6942");
    
    let clients = Arc::new(RwLock::new(
        HashMap::<UID, Client>::new()
    ));
    afs::create_dir_all("/tmp/revsh").await.expect("Could not create temp directory");
    afs::remove_file("/tmp/revsh/ipc").await.unwrap();
    
    let ipc_listener = UnixListener::bind("/tmp/revsh/ipc").expect("Could not create the ipc socket");
    
    let (global_sender, global_receiver) = broadcast::channel::<GlobalEvent>(100);

    loop {
        tokio::select! {
            a = listener.accept() => {
                let (socket, addr) = a.unwrap();
                
                let (mut reader, mut writer) = socket.into_split();

                let (out_sender, mut out_receiver) = mpsc::channel(100);
                
                let uid = new_uid();
                clients.write().unwrap().insert(uid, Client {
                    uid,
                    addr,
                    connected_since: Utc::now(),
                    out_events: out_sender,
                });
                global_sender.send(GlobalEvent::NewClient { uid }).unwrap();

                println!(
                    "New client #{uid} connected from {:?}:{:?}",
                    addr.ip(),
                    addr.port()
                );

                let gs = global_sender.clone();
                tokio::spawn(async move {
                    loop {
                        let Some(out_event) = out_receiver.recv().await
                            else { continue };
                        
                        match out_event {
                            OutClientEvent::SendMessage(mess) => {
                                match send_message_into(&mess, &mut writer)
                                    .await {
                                    Err(e) if 
                                        e.kind() == ErrorKind::UnexpectedEof ||
                                        e.kind() == io::ErrorKind::BrokenPipe ||
                                        e.kind() == ErrorKind::ConnectionReset
                                    => {
                                        break;
                                    },
                                    a => a.unwrap(),
                                };
                            }
                        }

                    }
                });

                let clis = Arc::clone(&clients);
                let global_sender = global_sender.clone();
                tokio::spawn(async move {
                    loop {
                        let mess: C2SMessage = match recv_message_from(&mut reader).await {
                            Err(e) if 
                                e.kind() == ErrorKind::UnexpectedEof ||
                                e.kind() == io::ErrorKind::BrokenPipe ||
                                e.kind() == ErrorKind::ConnectionReset
                            => {
                                gs.send(GlobalEvent::ClientDisconnect {
                                    uid
                                }).unwrap();
                                clis.write().unwrap().remove(&uid);
                                println!("Client {uid}({addr:?}) disconnected");
                                break;
                            },
                            a => a.unwrap(),
                        };

                        global_sender.send(GlobalEvent::ClientMessage {
                            sender: uid,
                            message: mess
                        }).unwrap();
                    }
                });
            },
            a = ipc_listener.accept() => {
                let (stream, addr) = a.unwrap();
                println!("New cli connection from {addr:?}");
                tokio::spawn(handle_cli_client(
                    Arc::clone(&clients), global_receiver.resubscribe(),
                    stream
                ));
            }
        };
    }
}

async fn handle_cli_client(
    clients: Arc<RwLock<HashMap<UID, Client>>>,
    mut global_receiver: broadcast::Receiver<GlobalEvent>,
    stream: UnixStream,
) -> anyhow::Result<()> {
    let (mut reader, mut writer) = stream.into_split();

    loop {
        tokio::select! {
            event = global_receiver.recv() => match event? {
                GlobalEvent::NewClient { uid } => {
                    let event = {
                        let clis = clients.read().unwrap();
                        let client = &clis[&uid];
                        OutCliMessage::ClientConnected {
                            info: OutCliUserInfo {
                                uid: client.uid,
                                addr: client.addr,
                                connected_at: client.connected_since,
                            }
                        }
                    };
                    send_message_into(&event, &mut writer).await?;
                },
                GlobalEvent::ClientDisconnect { uid } => {
                    send_message_into(
                        &OutCliMessage::ClientDisonnected { uid },
                        &mut writer
                    ).await?;
                },
                GlobalEvent::ClientMessage { sender, message } => {
                    send_message_into(
                        &OutCliMessage::ClientMessage { sender, message },
                        &mut writer
                    ).await?;
                },
            },
            
            message = recv_message_from::<InCliMessage, _>(&mut reader) => {
                let msg = match message {
                    Err(e) if
                        e.kind() == io::ErrorKind::UnexpectedEof ||
                        e.kind() == io::ErrorKind::BrokenPipe ||
                        e.kind() == io::ErrorKind::ConnectionReset
                    => {
                        break Ok(());
                    },
                    a => a.unwrap(),
                };
                match msg {
                    InCliMessage::ListClients {
                        page_size: _,
                        page_index: _,
                    } => {
                        let clis = clients.read().unwrap()
                            .iter().map(|(&uid, client)| {
                                OutCliUserInfo {
                                    uid,
                                    addr: client.addr,
                                    connected_at: client.connected_since,
                                }
                            }).collect::<Vec<_>>();
                        send_message_into(
                            &OutCliMessage::ClientList { users: clis },
                            &mut writer
                        ).await?;
                    },
                    InCliMessage::RenameClient {
                        uid: _,
                        new_name: _,
                    } => (),
                    InCliMessage::KickClient {
                        uid: _,
                    } => (),
                    InCliMessage::SendMessageTo {
                        target,
                        message,
                    } => {
                        let sender = clients.read().unwrap().get(&target)
                            .map(|a| a.out_events.clone());
                        if let Some(sender) = sender {
                            send_message_into(
                                &OutCliMessage::SendToFeeback(Ok(())),
                                &mut writer,
                            ).await.unwrap();
                            sender
                                .send(OutClientEvent::SendMessage(message)).await
                                .unwrap();
                        }
                        else {
                            send_message_into(
                                &OutCliMessage::SendToFeeback(Err(
                                    "Uknown client id".into()
                                )),
                                &mut writer,
                            ).await.unwrap();
                        }
                    },
                    InCliMessage::BroadcastMessage {
                        message,
                    } => {
                        let senders = clients.read().unwrap().iter()
                            .map(|(_, c)| c.out_events.clone())
                            .collect::<Vec<_>>();
                        for s in senders {
                            s.send(OutClientEvent::SendMessage(
                                message.clone()
                            )).await.unwrap();
                        }
                    },
                }
            }
        };
    }
}
