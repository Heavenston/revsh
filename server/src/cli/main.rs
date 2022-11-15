use tokio::net::UnixStream;
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
        
    }
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
        }
    }
  
    Ok(())
}
