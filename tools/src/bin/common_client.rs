use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use mio::{Interest, Token};
use mio::event::Event;
use mio::Registry;
use std::time::Instant;
use log::debug;
use log::error;
use log::info;
use log::warn;
use std::rc::Rc;
use std::vec::Vec;
use bytes::Bytes;

use tquic::PacketInfo;
use tquic::endpoint::Endpoint;
use tquic::Config;
use tquic::TransportHandler;
use tquic::Connection;
use tquic::PacketSendHandler;
use tquic::TlsConfig;

const QUIC_REMOTE: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9010);
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Client {
    endpoint: Endpoint,

    poll: mio::Poll,

    quic_socket: Rc<QuicSocket>,
}

impl Client {
    fn new() -> Result<Self> {
        let poll = mio::Poll::new()?;
        let registry = poll.registry();
        let quic_socket = Rc::new(QuicSocket::new(registry)?);

        let mut conf = Config::new()?;
        conf.set_max_idle_timeout(5000);

        let mut app_proto:Vec<Vec<u8>> = Vec::new();
        app_proto.push(b"http/0.9".to_vec());
        let tls_config = TlsConfig::new_client_config(app_proto, true)?;
        conf.set_tls_config(tls_config);
        let handler = ClientHandler::new(quic_socket.clone());
        let mut endpoint = Endpoint::new(Box::new(conf), false, Box::new(handler), quic_socket.clone());
        endpoint.connect(quic_socket.local_addr, QUIC_REMOTE, None, None, None, None)?;

        Ok(Client { 
            endpoint: endpoint,
            poll: poll,
            quic_socket: quic_socket,
        })
    }

    fn process_read_event(&mut self, event: &Event) -> Result<()> {
        match event.token() {
            Token(1) => {
                loop {
                    let mut buf = [0u8; 1500];
                    let (len, remote) = match self.quic_socket.socket.recv_from(&mut buf)
                    {
                        Ok(v) => v,
                        Err(e) => {
                            if e.kind() == std::io::ErrorKind::WouldBlock {
                                warn!("socket recv would block");
                                break;
                            }
                            return Err(format!("socket recv error: {:?}", e).into());
                        }
                    };
                    info!("socket recv {} bytes from {:?}", len, remote);

                    let pkt_buf = &mut buf[..len];
                    let pkt_info = PacketInfo {
                        src: remote,
                        dst: self.quic_socket.local_addr,
                        time: Instant::now(),
                    };

                    // Process the incoming packet.
                    match self.endpoint.recv(pkt_buf, &pkt_info) {
                        Ok(_) => {}
                        Err(e) => {
                            error!("recv failed: {:?}", e);
                            continue;
                        }
                    };
                }
                Ok(())
            }

            _ => Ok(()),
        }
    }
}

struct ClientHandler {
    quic_socket: Rc<QuicSocket>,
}

impl ClientHandler {
    fn new(socket: Rc<QuicSocket>) -> Self {
        Self {
            quic_socket: socket,
        }
    }
}

impl TransportHandler for ClientHandler {
    fn on_conn_created(&mut self, conn: &mut Connection) {
        info!("{} connection is created", conn.trace_id());
    }

    fn on_conn_established(&mut self, conn: &mut Connection) {
        info!(
            "{} connection is established, is_resumed: {}",
            conn.trace_id(),
            conn.is_resumed()
        );
        let request = Bytes::from(String::from("GET /resource\r\n"));

        let w = match conn.stream_write(
            0,
            request.clone(),
            true,
        ) {
            Ok(v) => v,
            Err(tquic::error::Error::StreamLimitError) => {
                error!("stream limit reached");
                0
            }
            Err(e) => {
                error!("failed to send request {:?}, error: {:?}", request, e);
                0
            }
        };
        info!("sent {} bytes on stream 0", w);
    }

    fn on_conn_closed(&mut self, conn: &mut Connection) {
        debug!("{} connection is closed", conn.trace_id());
    }

    fn on_stream_created(&mut self, conn: &mut Connection, stream_id: u64) {
        debug!("{} stream {} is created", conn.trace_id(), stream_id);
    }

    fn on_stream_readable(&mut self, conn: &mut Connection, stream_id: u64) {
        info!("{} stream {} is readable", conn.trace_id(), stream_id);
        let mut buf = [0u8; 1500];
        let mut buf_r: Vec<u8> = Vec::new();
        while let Ok((read, fin)) = conn.stream_read(stream_id, &mut buf) {
            buf_r.extend_from_slice(&buf[..read]);
            if fin {
                break;
            }
        }
        info!("Read {} bytes from stream {}, Content is: {:?}", buf_r.len(), stream_id, String::from_utf8(buf_r));
    }

    fn on_stream_writable(&mut self, conn: &mut Connection, stream_id: u64) {
        _ = conn.stream_want_write(stream_id, false);
    }

    fn on_stream_closed(&mut self, conn: &mut Connection, stream_id: u64) {
        debug!("{} stream {} is closed", conn.trace_id(), stream_id);
    }

    fn on_new_token(&mut self, _conn: &mut Connection, _token: Vec<u8>) {}
}

/// UDP socket wrapper for QUIC
pub struct QuicSocket {
    socket: mio::net::UdpSocket,

    local_addr: SocketAddr,
}

impl QuicSocket {
    fn new(registry: &Registry) -> Result<Self> {
        let local_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
        let mut socket = mio::net::UdpSocket::bind(local_addr)?;
        let local_addr = socket.local_addr()?;
        registry.register(&mut socket, Token(1), Interest::READABLE)?;

        Ok(Self {
            socket,
            local_addr,
        })
    }
}

impl PacketSendHandler for QuicSocket {
    fn on_packets_send(&self, pkts: &[(Vec<u8>, PacketInfo)]) -> tquic::Result<usize> {
        let mut count = 0;
        for (pkt, info) in pkts {
            if let Err(e) = self.socket.send_to(pkt, info.dst) {
                if e.kind() == std::io::ErrorKind::WouldBlock {
                    debug!("socket send would block");
                    return Ok(count);
                }
                return Err(tquic::Error::InvalidOperation(format!(
                    "socket send_to(): {:?}",
                    e
                )));
            }
            debug!("written {} bytes", pkt.len());
            count += 1;
        }
        Ok(count)
    }
}

fn main() -> Result<()> {
    env_logger::builder()
        .target(tquic_tools::log_target(&None)?)
        .filter_level(log::LevelFilter::Info)
        .format_timestamp_millis()
        .init();

    let mut client = Client::new()?;
    let mut events = mio::Events::with_capacity(1024);
    loop {
        client.endpoint.process_connections()?;

        client.poll.poll(&mut events, client.endpoint.timeout())?;

        // Process IO events
        for event in events.iter() {
            if event.is_readable() {
                client.process_read_event(event)?;
            }
        }

        // Process timeout events.
        // Note: Since `poll()` doesn't clearly tell if there was a timeout when it returns,
        // it is up to the endpoint to check for a timeout and deal with it.
        client.endpoint.on_timeout(Instant::now());
    }
}