use crate::{CONTROL_PORT, LOCAL_IP};
use alvr_common::{AnyhowToCon, ConResult, HandleTryAgain, ToCon, anyhow::Result, con_bail};
use alvr_session::{DscpTos, SocketBufferConfig};
use bincode::config;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    io::{Read, Write},
    marker::PhantomData,
    mem,
    net::{IpAddr, SocketAddr, TcpListener, TcpStream},
    time::{Duration, Instant},
};

// This corresponds to the length of the payload
const FRAMED_PREFIX_LENGTH: usize = mem::size_of::<u32>();

pub fn bind(
    timeout: Duration,
    port: u16,
    dscp: Option<DscpTos>,
    buffer_config: SocketBufferConfig,
) -> Result<TcpListener> {
    let socket = TcpListener::bind((LOCAL_IP, port))?.into();

    crate::set_socket_buffers(&socket, buffer_config).ok();

    crate::set_dscp(&socket, dscp);

    socket.set_read_timeout(Some(timeout))?;

    Ok(socket.into())
}

pub fn accept_from_server(
    listener: &TcpListener,
    server_ip: Option<IpAddr>,
    timeout: Duration,
) -> ConResult<(TcpStream, TcpStream)> {
    // Uses timeout set during bind()
    let (socket, server_address) = listener.accept().handle_try_again()?;

    if let Some(ip) = server_ip
        && server_address.ip() != ip
    {
        con_bail!(
            "Connected to wrong client: Expected: {ip}, Found {}",
            server_address.ip()
        );
    }

    socket.set_read_timeout(Some(timeout)).to_con()?;
    socket.set_nodelay(true).to_con()?;

    Ok((socket.try_clone().to_con()?, socket))
}

pub fn connect_to_client(
    timeout: Duration,
    client_ips: &[IpAddr],
    port: u16,
    buffer_config: SocketBufferConfig,
) -> ConResult<(TcpStream, TcpStream)> {
    let split_timeout = timeout / client_ips.len() as u32;

    let mut res = alvr_common::try_again();
    for ip in client_ips {
        res = TcpStream::connect_timeout(&SocketAddr::new(*ip, port), split_timeout)
            .handle_try_again();

        if res.is_ok() {
            break;
        }
    }
    let socket = res?.into();

    crate::set_socket_buffers(&socket, buffer_config).ok();
    socket.set_read_timeout(Some(timeout)).to_con()?;

    let socket = TcpStream::from(socket);

    socket.set_nodelay(true).to_con()?;

    Ok((socket.try_clone().to_con()?, socket))
}

fn framed_send<S: Serialize>(
    socket: &mut TcpStream,
    buffer: &mut Vec<u8>,
    packet: &S,
) -> Result<()> {
    buffer.resize(FRAMED_PREFIX_LENGTH, 0);

    let encoded_size = bincode::serde::encode_into_std_write(packet, buffer, config::standard())?;

    buffer[0..FRAMED_PREFIX_LENGTH].copy_from_slice(&(encoded_size as u32).to_le_bytes());

    socket.write_all(buffer)?;

    Ok(())
}

fn framed_recv<R: DeserializeOwned>(
    socket: &mut TcpStream,
    buffer: &mut Vec<u8>,
    recv_cursor: &mut Option<usize>,
    timeout: Duration,
) -> ConResult<R> {
    framed_recv_bytes(socket, buffer, recv_cursor, timeout)?;
    decode(&buffer[FRAMED_PREFIX_LENGTH..]).to_con()
}

// KiraVR: Decodes a packet payload. Trailing bytes are ignored, so a struct
// holding only the leading fields of a packet reads it even when later fields
// differ between versions. Used to understand a peer of another version just
// enough to tell the user what is wrong.
pub fn decode<R: DeserializeOwned>(bytes: &[u8]) -> Result<R> {
    let (packet, _) = bincode::serde::decode_from_slice(bytes, config::standard())?;
    Ok(packet)
}

// Reads one whole frame into buffer (prefix included).
fn framed_recv_bytes(
    socket: &mut TcpStream,
    buffer: &mut Vec<u8>,
    recv_cursor: &mut Option<usize>,
    timeout: Duration,
) -> ConResult {
    let deadline = Instant::now() + timeout;

    let recv_cursor_ref = if let Some(cursor) = recv_cursor {
        cursor
    } else {
        let mut payload_size_bytes = [0; FRAMED_PREFIX_LENGTH];

        loop {
            let count = socket.peek(&mut payload_size_bytes).handle_try_again()?;
            if count == FRAMED_PREFIX_LENGTH {
                break;
            } else if count == 0 {
                // KiraVR: End of stream, see MultiplexedTcpReader::recv.
                con_bail!("Connection closed by the peer");
            } else if Instant::now() > deadline {
                return alvr_common::try_again();
            }
        }

        let size = FRAMED_PREFIX_LENGTH + u32::from_le_bytes(payload_size_bytes) as usize;
        buffer.resize(size, 0);

        recv_cursor.insert(0)
    };

    loop {
        *recv_cursor_ref += socket
            .read(&mut buffer[*recv_cursor_ref..])
            .handle_try_again()?;

        if *recv_cursor_ref == buffer.len() {
            break;
        } else if Instant::now() > deadline {
            return alvr_common::try_again();
        }
    }

    *recv_cursor = None;

    Ok(())
}

pub struct ControlSocketSender<T> {
    pub(crate) inner: TcpStream,
    pub(crate) buffer: Vec<u8>,
    pub(crate) _phantom: PhantomData<T>,
}

impl<S: Serialize> ControlSocketSender<S> {
    pub fn send(&mut self, packet: &S) -> Result<()> {
        framed_send(&mut self.inner, &mut self.buffer, packet)
    }
}

pub struct ControlSocketReceiver<T> {
    pub(crate) inner: TcpStream,
    pub(crate) buffer: Vec<u8>,
    pub(crate) recv_cursor: Option<usize>,
    pub(crate) _phantom: PhantomData<T>,
}

impl<R: DeserializeOwned> ControlSocketReceiver<R> {
    pub fn recv(&mut self, timeout: Duration) -> ConResult<R> {
        framed_recv(
            &mut self.inner,
            &mut self.buffer,
            &mut self.recv_cursor,
            timeout,
        )
    }
}

pub fn get_server_listener(timeout: Duration) -> Result<TcpListener> {
    let listener = bind(timeout, CONTROL_PORT, None, SocketBufferConfig::default())?;

    Ok(listener)
}

// Proto-control-socket that can send and receive any packet. After the split, only the packets of
// the specified types can be exchanged
pub struct ProtoControlSocket {
    pub(crate) inner: TcpStream,
}

pub enum PeerType<'a> {
    AnyClient(Vec<IpAddr>),
    Server(&'a TcpListener),
}

impl ProtoControlSocket {
    pub fn connect_to(timeout: Duration, peer: PeerType<'_>) -> ConResult<(Self, IpAddr)> {
        let socket = match peer {
            PeerType::AnyClient(ips) => {
                connect_to_client(timeout, &ips, CONTROL_PORT, SocketBufferConfig::default())?.0
            }
            PeerType::Server(listener) => accept_from_server(listener, None, timeout)?.0,
        };

        let peer_ip = socket.peer_addr().to_con()?.ip();

        Ok((Self { inner: socket }, peer_ip))
    }

    pub fn send<S: Serialize>(&mut self, packet: &S) -> Result<()> {
        framed_send(&mut self.inner, &mut vec![], packet)
    }

    pub fn recv<R: DeserializeOwned>(&mut self, timeout: Duration) -> ConResult<R> {
        framed_recv(&mut self.inner, &mut vec![], &mut None, timeout)
    }

    // One packet as raw payload bytes, decode it with decode().
    pub fn recv_raw(&mut self, timeout: Duration) -> ConResult<Vec<u8>> {
        let mut buffer = vec![];
        framed_recv_bytes(&mut self.inner, &mut buffer, &mut None, timeout)?;
        Ok(buffer.split_off(FRAMED_PREFIX_LENGTH))
    }

    pub fn split<S: Serialize, R: DeserializeOwned>(
        self,
        timeout: Duration,
    ) -> Result<(ControlSocketSender<S>, ControlSocketReceiver<R>)> {
        self.inner.set_read_timeout(Some(timeout))?;

        Ok((
            ControlSocketSender {
                inner: self.inner.try_clone()?,
                buffer: vec![0; FRAMED_PREFIX_LENGTH],
                _phantom: PhantomData,
            },
            ControlSocketReceiver {
                inner: self.inner,
                buffer: vec![0; FRAMED_PREFIX_LENGTH],
                recv_cursor: None,
                _phantom: PhantomData,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alvr_packets::{
        ClientConnectionResult, ClientConnectionResultHead, ConnectionAcceptedInfo,
        IncompatibleNotice, StreamConfigHead,
    };

    fn encode<S: Serialize>(packet: &S) -> Vec<u8> {
        bincode::serde::encode_to_vec(packet, config::standard()).unwrap()
    }

    // The server must read the protocol ID from a full client packet through
    // the head, which is what makes mismatches explainable.
    #[test]
    fn head_reads_full_connection_result() {
        let full = ClientConnectionResult::ConnectionAccepted(Box::new(ConnectionAcceptedInfo {
            client_protocol_id: 0xdead_beef_1234_5678,
            platform_string: "Quest 3".into(),
            server_ip: "192.168.1.131".parse().unwrap(),
            streaming_capabilities: None,
        }));
        match decode::<ClientConnectionResultHead>(&encode(&full)).unwrap() {
            ClientConnectionResultHead::ConnectionAccepted(head) => {
                assert_eq!(head.client_protocol_id, 0xdead_beef_1234_5678)
            }
            ClientConnectionResultHead::ClientStandby => panic!("wrong variant"),
        }

        let standby = encode(&ClientConnectionResult::ClientStandby);
        assert!(matches!(
            decode::<ClientConnectionResultHead>(&standby).unwrap(),
            ClientConnectionResultHead::ClientStandby
        ));
    }

    #[test]
    fn notice_round_trips_and_normal_sessions_are_not_notices() {
        let notice = IncompatibleNotice {
            server_version: "21.0.0-dev15".into(),
            server_protocol: "21-dev15".into(),
            message: "This KiraVR app is older.".into(),
        };
        let bytes = encode(&notice.to_head().unwrap());
        let head = decode::<StreamConfigHead>(&bytes).unwrap();
        let read = IncompatibleNotice::from_head(&head).unwrap();
        assert_eq!(read.message, "This KiraVR app is older.");
        assert_eq!(read.server_protocol, "21-dev15");

        let normal = StreamConfigHead {
            session: r#"{"server_version":"21.0.0-dev14"}"#.into(),
        };
        assert!(IncompatibleNotice::from_head(&normal).is_none());
    }
}
