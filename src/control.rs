use crate::{
    error::{Error, Result},
    state::{Paths, Session, random_id},
};
use nix::{
    errno::Errno,
    poll::{PollFd, PollFlags, poll},
    sys::socket::{
        AddressFamily, SockFlag, SockType, UnixAddr, connect, getsockopt, socket, sockopt,
    },
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    io::{Read, Write},
    os::{
        fd::{AsFd, AsRawFd},
        unix::{fs::MetadataExt, net::UnixStream},
    },
    time::{Duration, Instant},
};

pub const VERSION: u32 = 1;
pub const LIMIT: usize = 65536;

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    Status,
    Close,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub session_id: String,
    pub request_id: String,
    pub operation: Operation,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub version: u32,
    pub session_id: String,
    pub request_id: String,
    pub session: Session,
    pub error: Option<String>,
}

pub fn send<T: Serialize>(stream: &mut UnixStream, value: &T, timeout: Duration) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    if bytes.len() > LIMIT {
        return Err(Error::new("control message exceeds size limit"));
    }
    let deadline = Instant::now() + timeout;
    while !bytes.is_empty() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::new("control write timed out"));
        }
        stream.set_write_timeout(Some(remaining))?;
        let count = stream.write(&bytes)?;
        if count == 0 {
            return Err(Error::new("control connection closed while writing"));
        }
        bytes.drain(..count);
    }
    Ok(())
}

pub fn receive<T: DeserializeOwned>(stream: &mut UnixStream, timeout: Duration) -> Result<T> {
    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::new("control read timed out"));
        }
        stream.set_read_timeout(Some(remaining))?;
        // Startup messages are acknowledged before the peer sends the next frame.
        let mut chunk = [0; 4096];
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            return Err(Error::new("control connection closed"));
        }
        if let Some(newline) = chunk[..count].iter().position(|byte| *byte == b'\n') {
            if newline + 1 != count {
                return Err(Error::new("unexpected data after control message"));
            }
            bytes.extend_from_slice(&chunk[..newline]);
            if bytes.len() >= LIMIT {
                return Err(Error::new("control message exceeds size limit"));
            }
            return Ok(serde_json::from_slice(&bytes)?);
        }
        bytes.extend_from_slice(&chunk[..count]);
        if bytes.len() >= LIMIT {
            return Err(Error::new("control message exceeds size limit"));
        }
    }
}

pub fn connect_session(paths: &Paths, id: &str, timeout: Duration) -> Result<UnixStream> {
    let path = paths.socket(id);
    let meta = std::fs::symlink_metadata(&path)?;
    use std::os::unix::fs::FileTypeExt;
    if !meta.file_type().is_socket()
        || meta.uid() != nix::unistd::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
    {
        return Err(Error::new("refusing unsafe control socket"));
    }
    #[cfg(not(target_vendor = "apple"))]
    let flags = SockFlag::SOCK_CLOEXEC | SockFlag::SOCK_NONBLOCK;
    #[cfg(target_vendor = "apple")]
    let flags = SockFlag::empty();
    let fd = socket(AddressFamily::Unix, SockType::Stream, flags, None)?;
    #[cfg(target_vendor = "apple")]
    {
        use nix::fcntl::{FcntlArg, FdFlag, OFlag, fcntl};
        fcntl(&fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
        fcntl(&fd, FcntlArg::F_SETFL(OFlag::O_NONBLOCK))?;
    }
    let addr = UnixAddr::new(&path)?;
    match connect(fd.as_raw_fd(), &addr) {
        Ok(()) => {}
        Err(Errno::EINPROGRESS) => {
            let mut fds = [PollFd::new(fd.as_fd(), PollFlags::POLLOUT)];
            if poll(&mut fds, timeout.as_millis().min(u16::MAX as u128) as u16)? == 0 {
                return Err(Error::new("control connect timed out"));
            }
            let error = getsockopt(&fd, sockopt::SocketError)?;
            if error != 0 {
                return Err(std::io::Error::from_raw_os_error(error).into());
            }
        }
        Err(e) => return Err(e.into()),
    }
    let stream = UnixStream::from(fd);
    stream.set_nonblocking(false)?;
    Ok(stream)
}

pub fn query(
    paths: &Paths,
    session: &Session,
    operation: Operation,
    timeout: Duration,
) -> Result<Session> {
    let mut stream = connect_session(paths, &session.id, timeout)?;
    let request = Request {
        version: VERSION,
        session_id: session.id.clone(),
        request_id: random_id()?,
        operation,
    };
    send(&mut stream, &request, timeout)?;
    let response: Response = receive(&mut stream, timeout)?;
    if response.version != VERSION
        || response.session_id != session.id
        || response.session.id != session.id
        || response.request_id != request.request_id
    {
        return Err(Error::new("control response identity or protocol mismatch"));
    }
    if let Some(error) = response.error {
        return Err(Error::new(error));
    }
    Ok(response.session)
}

pub fn respond(
    stream: &mut UnixStream,
    request: &Request,
    session: &Session,
    error: Option<String>,
) -> Result<()> {
    send(
        stream,
        &Response {
            version: VERSION,
            session_id: session.id.clone(),
            request_id: request.request_id.clone(),
            session: session.clone(),
            error,
        },
        Duration::from_millis(250),
    )
}
