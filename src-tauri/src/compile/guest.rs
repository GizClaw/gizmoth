//! Host side of the guest agent protocol: the shared message definitions plus
//! the hybrid-vsock handshake used to reach a VM's agent.
#[path = "../../../native/compile-agent/src/protocol.rs"]
pub mod protocol;

use std::{
    io::{self, BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::Duration,
};

/// Connects to a guest vsock port through OpenVMM's hybrid vsock socket.
pub fn connect(socket: &Path, port: u32, timeout: Duration) -> io::Result<UnixStream> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.write_all(format!("CONNECT {port}\n").as_bytes())?;
    // Read the reply byte by byte so no protocol bytes are buffered away.
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = io::Read::read(&mut stream, &mut byte)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "guest agent is not listening yet",
            ));
        }
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
        if line.len() > 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid vsock handshake",
            ));
        }
    }
    if !line.starts_with(b"OK ") {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            String::from_utf8_lossy(&line).into_owned(),
        ));
    }
    Ok(stream)
}

/// Sends `Hello` and returns the agent's reply.
pub fn hello(socket: &Path) -> io::Result<protocol::GuestMessage> {
    let mut stream = connect(socket, protocol::CONTROL_PORT, Duration::from_secs(5))?;
    protocol::write_json(&mut stream, &protocol::HostMessage::Hello)?;
    match protocol::read_frame(&mut stream)? {
        Some((protocol::JSON, payload)) => protocol::parse(&payload),
        _ => Err(io::Error::new(io::ErrorKind::InvalidData, "missing hello")),
    }
}

pub fn poweroff(socket: &Path) -> io::Result<()> {
    let mut stream = connect(socket, protocol::CONTROL_PORT, Duration::from_secs(5))?;
    protocol::write_json(&mut stream, &protocol::HostMessage::Poweroff)
}

/// Reads one line of console output for diagnostics without blocking callers.
pub fn console_tail(path: &Path, lines: usize) -> String {
    let Ok(file) = std::fs::File::open(path) else {
        return String::new();
    };
    let all: Vec<String> = BufReader::new(file).lines().map_while(Result::ok).collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}
