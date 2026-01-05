use bytes::buf;
use tquic::Connection;
use tquic::Endpoint;
use tquic::Config;
use tquic::TlsConfig;
use tquic::TransportHandler;
use tquic::PacketSendHandler;
use tquic::PacketInfo;

use std::net::SocketAddr;
use std::net::SocketAddrV4;
use std::str::FromStr;
use std::collections::VecDeque;
use std::collections::HashMap;
use std::os::unix::io::IntoRawFd;
use std::rc::Rc;
use std::cell::RefCell;
use std::time::Duration;
use std::time::Instant;
use bytes::Bytes;
use bytes::BytesMut;
use env_logger::Target;
use log::*;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use pyo3::types::PyDelta;

#[pyclass(unsendable)]
struct ClientEndpoint {
    quic_endpoint: Endpoint,
    quic_connection: Option<u64>,
    quic_sock: Rc<ClientPacketSender>,
    recv_queue: Rc<RefCell<VecDeque<Bytes>>>,
    client_stream_id: u64,
}

#[pymethods]
impl ClientEndpoint {
    fn connect(&mut self, server_addr: String) -> PyResult<()> {
        let remote_addr = server_addr.parse().map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(format!("Invalid server address: {}", e)))?;
        let local_addr = self.quic_sock.sock.local_addr().map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Failed to get local address: {}", e)))?;
        let conn = self.quic_endpoint.connect(local_addr, remote_addr, Some("Test"), None, None, None).map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Failed to connect: {}", e)))?;
        self.quic_connection = Some(conn);
        Ok(())
    }

    fn send(&mut self, data: &PyBytes) -> PyResult<usize> {
        if self.quic_connection.is_none() {
            return Err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>("No active connection"));
        }
        let conn = self.quic_endpoint.conn_get_mut(self.quic_connection.unwrap()).unwrap();
        let w = match conn.stream_write(self.client_stream_id, Bytes::copy_from_slice(data.as_bytes()), true) {
            Ok(w) => {
                debug!("wrote {} bytes to stream {}", w, self.client_stream_id);
                self.client_stream_id += 4;
                w
            }
            Err(_) => {
                // println!("Failed to write to stream: {}", e);
                0
            }
        };
        Ok(w)
    }

    fn send_sqp(&mut self, data: &PyBytes) -> PyResult<usize> {
        if self.quic_connection.is_none() {
            return Err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>("No active connection"));
        }
        let conn = self.quic_endpoint.conn_get_mut(self.quic_connection.unwrap()).unwrap();
        let w = match conn.stream_write_sqp(self.client_stream_id, Bytes::copy_from_slice(data.as_bytes()), true) {
            Ok(w) => {
                debug!("wrote {} bytes to stream {}", w, self.client_stream_id);
                self.client_stream_id += 4;
                w
            }
            Err(e) => {
                println!("Failed to write to stream: {}", e);
                0
            }
        };
        Ok(w)
    }

    fn get_sqp_bandwidth_estimate(&mut self) -> PyResult<Option<u64>> {
        let conn = match self.quic_endpoint.conn_get_mut(self.quic_connection.unwrap()) {
            Some(conn) => Some(conn),
            None => None,
        };
        if conn.is_none() {
            println!("Connection does not exist");
            return Ok(None);
        }
        let bandwidth_estimate = match conn.unwrap().get_sqp_bandwidth_estimate() {
            Ok(bw) => bw,
            Err(e) => {
                error!("Failed to get SQP bandwidth estimate: {}", e);
                return Ok(None);
            }
            
        };
        Ok(Some(bandwidth_estimate))
    }

    fn recv<'a>(&mut self, py: Python<'a>) -> PyResult<Option<&'a PyBytes>> {
        if let Some(data_vec) = self.recv_queue.borrow_mut().pop_front() {
            let py_bytes = PyBytes::new(py, &data_vec.as_ref());
            Ok(Some(py_bytes))
        } else {
            Ok(None)
        }
    }

    fn check_recv_queue(&self) -> PyResult<usize> {
        Ok(self.recv_queue.borrow().len())
    }

    fn quic_connection_close(&mut self) {
        if self.quic_connection.is_some() {
            match self.quic_endpoint.conn_get_mut(self.quic_connection.unwrap()) {
                Some(conn) => {
                    let _ = conn.close(true, 0, b"Nomal Closure");
                }
                None => {
                    error!("Connection does not exist");
                }
            } 
        }
        
    }

    fn quic_endpoint_recv(&mut self, data: &PyBytes, addr_tuple: &PyAny) {
        if let Ok((ip_str, port)) = addr_tuple.extract::<(String, u16)>() {
            // 拼接成 Rust 能够识别的格式 "IP:Port"
            let addr_string = format!("{}:{}", ip_str, port);
            match SocketAddr::from_str(&addr_string) {
                Ok(addr) => {
                    let info = PacketInfo {
                        src: addr,
                        dst: self.quic_sock.sock.local_addr().unwrap(),
                        time: Instant::now(),
                    };
                    let _ = self.quic_endpoint.recv(&mut data.as_bytes().to_vec(), &info);
                    // println!("接收到数据包，来自 {}", addr_string);
                    
                },
                Err(e) => {
                    error!("无效的 IPv4 格式: {}", e);
                }
            }
        }

    }
    
    fn quic_process_connections(&mut self) {
        match self.quic_endpoint.process_connections() {
            Ok(_) => {},
            Err(e) => {
                error!("error processing connections: {:?}", e);
            }
        }
    }

    fn quic_on_timeout(&mut self) {
        self.quic_endpoint.on_timeout(Instant::now());
    }

    fn get_quic_timeout<'a>(&self, py: Python<'a>) -> PyResult<&'a PyDelta> {
        let time = match self.quic_endpoint.timeout() {
            Some(dur) => dur,
            None => Duration::from_secs(0),
        };
        let py_delta = PyDelta::new(
            py,
            0,                                   // days
            time.as_secs() as i32,        // seconds
            time.subsec_micros() as i32,  // microseconds
            true                                 // normalize (自动进位)
        )?;
        Ok(py_delta)
    }

    fn get_socket_fd(&self) -> PyResult<u64> {
        let socket_clone = self.quic_sock.sock.try_clone().map_err(|e| {
            pyo3::exceptions::PyOSError::new_err(format!("Clone failed: {}", e))
        })?;
        let fd = socket_clone.into_raw_fd();
        Ok(fd as u64)
    }
}

struct ClientPacketSender {
    sock: std::net::UdpSocket,
}

impl ClientPacketSender {
    fn new(sock: std::net::UdpSocket) -> Self {
        ClientPacketSender { sock }
    }
    
    fn send_to(&self, buf: &[u8], src: std::net::SocketAddr, dst: std::net::SocketAddr) -> std::io::Result<usize> {
        match self.sock.send_to(buf, dst) {
            Ok(len) => Ok(len),
            Err(e) => {
                error!("failed to send packet from {:?} to {:?}: {:?}", src, dst, e);
                Err(e)
            }
        } 
    }
}

impl PacketSendHandler for ClientPacketSender {
    fn on_packets_send(&self, pkts: &[(Vec<u8>, PacketInfo)]) -> tquic::Result<usize> {
        let mut count = 0;
        for (pkt, info) in pkts {
            if let Err(e) = self.send_to(pkt, info.src, info.dst) {
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

struct ClientHandler{
    py_callbacks: PyQuicCallBacks,
    recv_queue: Rc<RefCell<VecDeque<Bytes>>>,
}

impl TransportHandler for ClientHandler {
    fn on_conn_created(&mut self, conn: &mut Connection) {
        println!("Connection created");
        if self.py_callbacks.on_conn_create_callback.is_some() {
            Python::with_gil(|py| {
                let callback = self.py_callbacks.on_conn_create_callback.as_ref().unwrap();
                let _ = callback.call1(py, (conn.index().unwrap(),));
            });
        }
    }

    fn on_conn_established(&mut self, conn: &mut Connection) {
        println!("Connection established");
        if self.py_callbacks.on_conn_established_callback.is_some() {
            Python::with_gil(|py| {
                let callback = self.py_callbacks.on_conn_established_callback.as_ref().unwrap();
                let _ = callback.call1(py, (conn.index().unwrap(),));
            });
        }
    }
    
    fn on_conn_closed(&mut self, conn: &mut Connection) {
        if self.py_callbacks.on_conn_closed_callback.is_some() {
            Python::with_gil(|py| {
                let callback = self.py_callbacks.on_conn_closed_callback.as_ref().unwrap();
                let _ = callback.call1(py, (conn.index().unwrap(),));
            });
        }
    }

    fn on_stream_created(&mut self, conn: &mut Connection, stream_id: u64) {
        if self.py_callbacks.on_stream_created_callback.is_some() {
            Python::with_gil(|py| {
                let callback = self.py_callbacks.on_stream_created_callback.as_ref().unwrap();
                let _ = callback.call1(py, (conn.index().unwrap(), stream_id));
            });
        }
    }

    fn on_stream_readable(&mut self, conn: &mut Connection, stream_id: u64) {
        let mut buf: [u8; 65535]  = [0; 65535];
        let mut buf_full = BytesMut::with_capacity(655350);
        while let Ok((r, fin)) = conn.stream_read(stream_id, &mut buf) {
            buf_full.extend_from_slice(&buf[..r]);
            if fin {
                self.recv_queue.borrow_mut().push_back(buf_full.clone().freeze());
                break;
            }
        }
        if self.py_callbacks.on_stream_readable_callback.is_some() {
            let data = buf_full.freeze();
            Python::with_gil(|py| {
                let callback = self.py_callbacks.on_stream_readable_callback.as_ref().unwrap();
                let py_bytes = PyBytes::new(py, &data.as_ref());
                match callback.call1(py, (conn.index().unwrap(), stream_id, py_bytes)){
                    Ok(response) => {
                        if response.is_none(py) {
                            return;
                        }
                        if let Ok(response_bytes) = response.extract::<Vec<u8>>(py) {
                            println!("[Rust] 从 Python 拿到 {} 字节", response_bytes.len());
                            // match conn.stream_write(stream_id, Bytes::copy_from_slice(&response_bytes), true) {
                            //     Ok(written) => debug!("[Rust] 成功发送 {} 字节", written),
                            //     Err(e) => error!("[Rust] 发送失败: {:?}", e),
                            // }
                        } else {
                            error!("[Rust] Python 回调返回了非 bytes 类型的数据！");
                        }
                    },
                    Err(e) => {
                        error!("Error in on_stream_readable_callback: {:?}", e);
                    }
                }
            });
        }
    }

    fn on_stream_writable(&mut self, conn: &mut Connection, stream_id: u64) {
        if self.py_callbacks.on_stream_writable_callback.is_some() {
            Python::with_gil(|py| {
                let callback = self.py_callbacks.on_stream_writable_callback.as_ref().unwrap();
                let _ = callback.call1(py, (conn.index().unwrap(), stream_id));
            });
        }
        let _ = conn.stream_want_write(stream_id, false);
    }

    fn on_stream_closed(&mut self, conn: &mut Connection, stream_id: u64) {
        if self.py_callbacks.on_stream_closed_callback.is_some() {
            Python::with_gil(|py| {
                let callback = self.py_callbacks.on_stream_closed_callback.as_ref().unwrap();
                let _ = callback.call1(py, (conn.index().unwrap(), stream_id));
            });
        }
    }

    fn on_new_token(&mut self, conn: &mut Connection, token: Vec<u8>) {
        if self.py_callbacks.on_new_token_callback.is_some() {
            Python::with_gil(|py| {
                let callback = self.py_callbacks.on_new_token_callback.as_ref().unwrap();
                let py_bytes = PyBytes::new(py, &token);
                let _ = callback.call1(py, (conn.index().unwrap(), py_bytes));
            });
        }
    }
}

#[pyclass]
#[derive(Clone)]
struct PyQuicCallBacks {
    on_conn_create_callback: Option<Py<PyAny>>,
    on_conn_established_callback: Option<Py<PyAny>>,
    on_conn_closed_callback: Option<Py<PyAny>>,
    on_stream_created_callback: Option<Py<PyAny>>,
    on_stream_readable_callback: Option<Py<PyAny>>,
    on_stream_writable_callback: Option<Py<PyAny>>,
    on_stream_closed_callback: Option<Py<PyAny>>,
    on_new_token_callback: Option<Py<PyAny>>,
}

#[pymethods]
#[allow(non_local_definitions)]
impl PyQuicCallBacks {
    #[new]
    fn new() -> Self {
        PyQuicCallBacks {
            on_conn_create_callback: None,
            on_conn_established_callback: None,
            on_conn_closed_callback: None,
            on_stream_created_callback: None,
            on_stream_readable_callback: None,
            on_stream_writable_callback: None,
            on_stream_closed_callback: None,
            on_new_token_callback: None,
        }
    }

    fn set_on_conn_created_callback(&mut self, callback: Py<PyAny>) {
        self.on_conn_create_callback = Some(callback);
    }

    fn set_on_conn_established_callback(&mut self, callback: Py<PyAny>) {
        self.on_conn_established_callback = Some(callback);
    }

    fn set_on_conn_closed_callback(&mut self, callback: Py<PyAny>) {
        self.on_conn_closed_callback = Some(callback);
    }

    fn set_on_stream_created_callback(&mut self, callback: Py<PyAny>) {
        self.on_stream_created_callback = Some(callback);
    }

    fn set_on_stream_readable_callback(&mut self, callback: Py<PyAny>) {
        self.on_stream_readable_callback = Some(callback);
    }

    fn set_on_stream_writable_callback(&mut self, callback: Py<PyAny>) {
        self.on_stream_writable_callback = Some(callback);
    }

    fn set_on_stream_closed_callback(&mut self, callback: Py<PyAny>) {
        self.on_stream_closed_callback = Some(callback);
    }

    fn set_on_new_token_callback(&mut self, callback: Py<PyAny>) {
        self.on_new_token_callback = Some(callback);
    }
}

#[pyclass(unsendable)]
struct ServerEndpoint {
    quic_endpoint: Endpoint,
    quic_sock: Rc<ClientPacketSender>,
}

#[pymethods]
impl ServerEndpoint {
    fn send(&mut self, conn_id: u64, stream_id: u64, data: &PyBytes) -> PyResult<usize> {
        let conn = match self.quic_endpoint.conn_get_mut(conn_id) {
            Some(conn) => Some(conn),
            None => None,
        };
        if conn.is_none() {
            println!("Connection does not exist");
            return Ok(0);
        }
        let w = match conn.unwrap().stream_write(stream_id, Bytes::copy_from_slice(data.as_bytes()), true) {
            Ok(w) => {
                debug!("wrote {} bytes to stream {}", w, stream_id);
                w
            }
            Err(e) => {
                error!("Failed to write to stream: {}", e);
                0
            }
        };
        Ok(w)
    }

    fn send_sqp(&mut self, conn_id: u64, stream_id: u64, data: &PyBytes) -> PyResult<usize> {
        let conn = match self.quic_endpoint.conn_get_mut(conn_id) {
            Some(conn) => Some(conn),
            None => None,
        };
        if conn.is_none() {
            println!("Connection does not exist");
            return Ok(0);
        }
        let w = match conn.unwrap().stream_write_sqp(stream_id, Bytes::copy_from_slice(data.as_bytes()), true) {
            Ok(w) => {
                debug!("wrote {} bytes to stream {}", w, stream_id);
                w
            }
            Err(e) => {
                error!("Failed to write to stream: {}", e);
                0
            }
        };
        Ok(w)
    }

    fn get_sqp_bandwidth_estimate(&mut self, conn_id: u64) -> PyResult<Option<u64>> {
        let conn = match self.quic_endpoint.conn_get_mut(conn_id) {
            Some(conn) => Some(conn),
            None => None,
        };
        if conn.is_none() {
            println!("Connection does not exist");
            return Ok(None);
        }
        let bandwidth_estimate = match conn.unwrap().get_sqp_bandwidth_estimate() {
            Ok(bw) => bw,
            Err(e) => {
                error!("Failed to get SQP bandwidth estimate: {}", e);
                return Ok(None);
            }
            
        };
        Ok(Some(bandwidth_estimate))
    }

    fn quic_endpoint_recv(&mut self, data: &PyBytes, addr_tuple: &PyAny) {
        if let Ok((ip_str, port)) = addr_tuple.extract::<(String, u16)>() {
            // 拼接成 Rust 能够识别的格式 "IP:Port"
            let addr_string = format!("{}:{}", ip_str, port);
            match SocketAddr::from_str(&addr_string) {
                Ok(addr) => {
                    let info = PacketInfo {
                        src: addr,
                        dst: self.quic_sock.sock.local_addr().unwrap(),
                        time: Instant::now(),
                    };
                    let _ = self.quic_endpoint.recv(&mut data.as_bytes().to_vec(), &info);
                    // println!("接收到数据包，来自 {}", addr_string);
                    
                },
                Err(e) => {
                    error!("无效的 IPv4 格式: {}", e);
                }
            }
        }

    }
    
    fn quic_process_connections(&mut self) {
        match self.quic_endpoint.process_connections() {
            Ok(_) => {},
            Err(e) => {
                error!("error processing connections: {:?}", e);
            }
        }
    }

    fn quic_on_timeout(&mut self) {
        self.quic_endpoint.on_timeout(Instant::now());
    }

    fn get_quic_timeout<'a>(&self, py: Python<'a>) -> PyResult<&'a PyDelta> {
        let time = match self.quic_endpoint.timeout() {
            Some(dur) => dur,
            None => Duration::from_secs(0),
        };
        let py_delta = PyDelta::new(
            py,
            0,                                   // days
            time.as_secs() as i32,        // seconds
            time.subsec_micros() as i32,  // microseconds
            true                                 // normalize (自动进位)
        )?;
        Ok(py_delta)
    }

    fn get_socket_fd(&self) -> PyResult<u64> {
        let socket_clone = self.quic_sock.sock.try_clone().map_err(|e| {
            pyo3::exceptions::PyOSError::new_err(format!("Clone failed: {}", e))
        })?;
        let fd = socket_clone.into_raw_fd();
        Ok(fd as u64)
    }


}

struct ServerHandler{
    py_callbacks: PyQuicCallBacks,
    recv_map: HashMap<u64, BytesMut>,
}

impl TransportHandler for ServerHandler {
    fn on_conn_created(&mut self, conn: &mut Connection) {
        println!("Connection created");
        if self.py_callbacks.on_conn_create_callback.is_some() {
            Python::with_gil(|py| {
                let callback = self.py_callbacks.on_conn_create_callback.as_ref().unwrap();
                let _ = callback.call1(py, (conn.index().unwrap(),));
            });
        }
    }

    fn on_conn_established(&mut self, conn: &mut Connection) {
        println!("Connection established");
        if self.py_callbacks.on_conn_established_callback.is_some() {
            Python::with_gil(|py| {
                let callback = self.py_callbacks.on_conn_established_callback.as_ref().unwrap();
                let _ = callback.call1(py, (conn.index().unwrap(),));
            });
        }
    }
    
    fn on_conn_closed(&mut self, conn: &mut Connection) {
        if self.py_callbacks.on_conn_closed_callback.is_some() {
            Python::with_gil(|py| {
                let callback = self.py_callbacks.on_conn_closed_callback.as_ref().unwrap();
                let _ = callback.call1(py, (conn.index().unwrap(),));
            });
        }
    }

    fn on_stream_created(&mut self, conn: &mut Connection, stream_id: u64) {
        if self.py_callbacks.on_stream_created_callback.is_some() {
            Python::with_gil(|py| {
                let callback = self.py_callbacks.on_stream_created_callback.as_ref().unwrap();
                let _ = callback.call1(py, (conn.index().unwrap(), stream_id));
            });
        }
    }

    fn on_stream_readable(&mut self, conn: &mut Connection, stream_id: u64) {
        let mut buf: [u8; 65535]  = [0; 65535];
        while let Ok((r, fin)) = conn.stream_read(stream_id, &mut buf) {
            if !self.recv_map.contains_key(&stream_id){
                let mut data_seg = BytesMut::with_capacity(655350);
                data_seg.extend_from_slice(&buf[..r]);
                self.recv_map.insert(stream_id, data_seg);
            } else {
                let existing_data = self.recv_map.get_mut(&stream_id).unwrap();
                existing_data.extend_from_slice(&buf[..r]);
            }
            debug!("Server received {} bytes on stream {}", r, stream_id);
            if fin {
                if self.py_callbacks.on_stream_readable_callback.is_some() {
                    let data = self.recv_map.remove(&stream_id).unwrap().freeze();
                    println!("Server received {} bytes on stream {}", data.len(), stream_id);
                    Python::with_gil(|py| {
                        let callback = self.py_callbacks.on_stream_readable_callback.as_ref().unwrap();
                        let py_bytes = PyBytes::new(py, &data.as_ref());
                        match callback.call1(py, (conn.index().unwrap(), stream_id, py_bytes)){
                            Ok(response) => {
                                if response.is_none(py) {
                                    return;
                                }
                                if let Ok(response_bytes) = response.extract::<Vec<u8>>(py) {
                                debug!("[Rust] 从 Python 拿到 {} 字节，准备发送...", response_bytes.len());
                                match conn.stream_write(stream_id, Bytes::copy_from_slice(&response_bytes), true) {
                                    Ok(written) => debug!("[Rust] 成功发送 {} 字节", written),
                                    Err(e) => error!("[Rust] 发送失败: {:?}", e),
                                }
                                } else {
                                    error!("[Rust] Python 回调返回了非 bytes 类型的数据！");
                                }
                            },
                            Err(e) => {
                                error!("Error in on_stream_readable_callback: {:?}", e);
                            }
                        }
                    });
                }
                break;
            }
        }
        
    }

    fn on_stream_writable(&mut self, conn: &mut Connection, stream_id: u64) {
        if self.py_callbacks.on_stream_writable_callback.is_some() {
            Python::with_gil(|py| {
                let callback = self.py_callbacks.on_stream_writable_callback.as_ref().unwrap();
                let _ = callback.call1(py, (conn.index().unwrap(), stream_id));
            });
        }
        let _ = conn.stream_want_write(stream_id, false);
    }

    fn on_stream_closed(&mut self, conn: &mut Connection, stream_id: u64) {
        if self.py_callbacks.on_stream_closed_callback.is_some() {
            Python::with_gil(|py| {
                let callback = self.py_callbacks.on_stream_closed_callback.as_ref().unwrap();
                let _ = callback.call1(py, (conn.index().unwrap(), stream_id));
            });
        }
    }

    fn on_new_token(&mut self, conn: &mut Connection, token: Vec<u8>) {
        if self.py_callbacks.on_new_token_callback.is_some() {
            Python::with_gil(|py| {
                let callback = self.py_callbacks.on_new_token_callback.as_ref().unwrap();
                let py_bytes = PyBytes::new(py, &token);
                let _ = callback.call1(py, (conn.index().unwrap(), py_bytes));
            });
        }
    }
}

#[pyfunction]
fn quic_client_create(pycallbacks: PyQuicCallBacks) -> PyResult<ClientEndpoint> {
    let mut config = Config::new().map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Failed to create config: {}", e)))?;
    config.set_max_idle_timeout(60000);
    config.set_initial_max_streams_bidi(20000);
    config.set_initial_max_streams_uni(20000);
    let tls_config = TlsConfig::new_client_config(vec![b"http/0.9".to_vec()], true).unwrap();
    config.set_tls_config(tls_config);
    config.set_congestion_control_algorithm(tquic::CongestionControlAlgorithm::SQP);

    let sock = std::net::UdpSocket::bind("0.0.0.0:0").map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Failed to bind socket: {}", e)))?;
    sock.set_nonblocking(true).map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Failed to set non-blocking: {}", e)))?;
    let recv_queue: Rc<RefCell<VecDeque<Bytes>>> = Rc::new(RefCell::new(VecDeque::new()));
    let handler = ClientHandler{
        py_callbacks: pycallbacks,
        recv_queue: recv_queue.clone(),
    };
    let packetsender = Rc::new(ClientPacketSender::new(sock));

    let endpoint = Endpoint::new(Box::new(config), false, Box::new(handler), packetsender.clone());
    Ok(ClientEndpoint{ 
        quic_endpoint: endpoint, 
        quic_connection: None,
        recv_queue: recv_queue,
        quic_sock: packetsender,
        client_stream_id: 0,
    })
}

#[pyfunction]
fn quic_server_create(bind_addr: String, pycallbacks: PyQuicCallBacks) -> PyResult<ServerEndpoint> {
    let mut config = Config::new().map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Failed to create config: {}", e)))?;
    config.set_max_idle_timeout(60000);
    config.set_initial_max_streams_bidi(20000);
    config.set_initial_max_streams_uni(20000);
    let mut tls_config = TlsConfig::new_client_config(vec![b"http/0.9".to_vec()], true).unwrap();
    tls_config.set_certificate_file("./cert.crt").map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Failed to set certificate file: {}", e)))?;
    tls_config.set_private_key_file("./cert.key").map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Failed to set private key file: {}", e)))?;
    config.set_tls_config(tls_config);

    let parse_addr: SocketAddrV4 = bind_addr.parse().map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(format!("Invalid bind address: {}", e)))?;
    let sock = std::net::UdpSocket::bind(parse_addr).map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Failed to bind socket: {}", e)))?;
    sock.set_nonblocking(true).map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Failed to set non-blocking: {}", e)))?;
    let handler = ServerHandler{
        py_callbacks: pycallbacks,
        recv_map: HashMap::new(),
    };
    let packetsender = Rc::new(ClientPacketSender::new(sock));

    let endpoint = Endpoint::new(Box::new(config), true, Box::new(handler), packetsender.clone());
    Ok(ServerEndpoint{ 
        quic_endpoint: endpoint, 
        quic_sock: packetsender,
    })
}

#[pymodule]
fn tquic_py(_py: Python<'_>, m: &PyModule) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(quic_client_create, m)?)?;
    m.add_function(wrap_pyfunction!(quic_server_create, m)?)?;
    m.add_class::<ClientEndpoint>()?;
    m.add_class::<ServerEndpoint>()?;
    m.add_class::<PyQuicCallBacks>()?;
    Ok(())
}
