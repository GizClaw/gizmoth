//! Host side of the guest agent protocol: the shared message definitions plus
//! the hybrid-vsock handshake used to reach a VM's agent.
#[path = "../../../native/compile-agent/src/protocol.rs"]
pub mod protocol;

use std::{
    io::{self, Read, Seek, SeekFrom, Write},
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

const MAX_CONSOLE_TAIL: u64 = 64 * 1024;
const MAX_CONSOLE_LINE: usize = 4096;

/// Reads bounded recent console bytes, tolerating binary/partial UTF-8 output.
pub fn console_tail(path: &Path, lines: usize) -> String {
    if lines == 0 {
        return String::new();
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let Ok(meta) = file.metadata() else {
        return String::new();
    };
    let start = meta.len().saturating_sub(MAX_CONSOLE_TAIL);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut tail = Vec::new();
    // Limit even if the console keeps growing after metadata was read.
    if file.take(MAX_CONSOLE_TAIL).read_to_end(&mut tail).is_err() {
        return String::new();
    }
    let tail = tail.strip_suffix(b"\n").unwrap_or(&tail);
    if tail.is_empty() {
        return String::new();
    }
    let mut recent: Vec<_> = tail
        .rsplit(|b| *b == b'\n')
        .take(lines)
        .map(|line| {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            String::from_utf8_lossy(&line[line.len().saturating_sub(MAX_CONSOLE_LINE)..])
                .into_owned()
        })
        .collect();
    recent.reverse();
    recent.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_tail_keeps_recent_lines_after_invalid_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("console");
        std::fs::write(&path, b"old\n\xffbroken\r\nnewest\n").unwrap();
        assert_eq!(console_tail(&path, 2), "\u{fffd}broken\nnewest");
        assert_eq!(console_tail(&path, 1), "newest");
        assert_eq!(console_tail(&path, 0), "");
        assert_eq!(console_tail(&dir.path().join("missing"), 3), "");
    }

    #[test]
    fn console_tail_bounds_large_files_and_unterminated_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("console");
        let mut file = std::fs::File::create(&path).unwrap();
        // Sparse prefix proves the reader seeks near EOF rather than loading it.
        file.set_len(1 << 30).unwrap();
        file.seek(SeekFrom::End(0)).unwrap();
        file.write_all(&vec![b'x'; MAX_CONSOLE_TAIL as usize * 2])
            .unwrap();
        file.write_all(b"end").unwrap();
        let tail = console_tail(&path, usize::MAX);
        assert_eq!(tail.len(), MAX_CONSOLE_LINE);
        assert!(tail.ends_with("end"));
        assert!(!tail.contains('\0'));
        file.write_all(b"\nlast\n").unwrap();
        assert_eq!(console_tail(&path, 1), "last");
        assert!(console_tail(&path, usize::MAX).len() <= MAX_CONSOLE_LINE + 5);
    }
}
