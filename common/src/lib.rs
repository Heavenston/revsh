use serde::{ Deserialize, Serialize };
use std::sync::atomic;
use std::marker::Unpin;
use std::io::Error as IoError;
use std::mem::size_of;
use nanorand::Rng;
use tokio::io::{ AsyncWrite, AsyncRead, AsyncWriteExt, AsyncReadExt };

pub static UID_COUNTER: atomic::AtomicU32 = atomic::AtomicU32::new(0);

pub type UID = u32;
pub fn new_uid() -> UID {
    nanorand::tls_rng().generate::<UID>() % 0xFFFF
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum S2CMessage {
    Execute {
        pid: UID,
        exe: String,
        args: Vec<String>,
        print_output: bool,
        client_only: bool,
    },
    KillProcess {
        pid: UID,
    },
    Input {
        target_pid: UID,
        data: Box<[u8]>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum C2SMessage {
    Hello {
        mac_address: mac_address::MacAddress,
        hostname: String,
    },
    ProcessOutput {
        pid: UID,
        data: Box<[u8]>,
    },
    ProcessStopped {
        pid: UID,
        exit_code: i32,
    },
}

pub async fn send_message_into(
    message: &impl Serialize,
    mut writer: impl AsyncWrite + Unpin
) -> Result<(), IoError> {
    let srds = bincode::serialize(message).unwrap();

    writer.write_all(&srds.len().to_ne_bytes()).await?;
    writer.write_all(&srds).await?;
    
    Ok(())
}

pub async fn recv_message_from<T: for<'a> Deserialize<'a>, R: AsyncRead + Unpin>(
    mut reader: R
) -> Result<T, IoError> {
    let mut len_buf = [0u8; size_of::<usize>()];
    reader.read_exact(&mut len_buf).await?;
    let len = usize::from_ne_bytes(len_buf);
    let mut buffer = vec![0u8; len];
    reader.read_exact(&mut buffer).await?;

    Ok(bincode::deserialize(&buffer).unwrap())
}
