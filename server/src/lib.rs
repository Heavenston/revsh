use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use std::mem::size_of;
use std::sync::{Arc, Mutex};
use std::io::{ self, BufRead };
use std::net::SocketAddr;
use std::collections::HashMap;
use tokio::io::{ AsyncReadExt, AsyncWriteExt };
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use chrono::{ Utc, DateTime };
use std::time::Instant;
use revsh_common::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OutClientEvent {
    SendMessage(S2CMessage),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InClientEvent {
    Message(C2SMessage),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GlobalInEvent {
    NewClient {
        id: UID,
        addr: SocketAddr,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InCliMessage {
    ListClients {
        page_size: u32,
        page_index: u32,
    },
    RenameClient {
        uid: UID,
        new_name: String,
    },
    KickClient {
        uid: UID,
    },
    SendMessageTo {
        target: UID,
        message: S2CMessage,
    },
    BroadcastMessage {
        message: S2CMessage,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutCliUserInfo {
    pub uid: UID,
    pub addr: SocketAddr,
    pub connected_at: DateTime<Utc>,
    pub mac_address: Option<mac_address::MacAddress>,
    pub hostname: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OutCliMessage {
    ClientList {
        users: Vec<OutCliUserInfo>,
    },
    SendToFeeback(Result<(), String>),

    ClientConnected {
        info: OutCliUserInfo,
    },
    ClientDisonnected {
        uid: UID,
    },
    ClientMessage {
        sender: UID,
        message: C2SMessage,
    }
}
