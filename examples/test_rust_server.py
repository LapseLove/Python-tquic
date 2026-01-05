import tquic_py
import socket
import selectors

def on_conn_created(conn_id):
    print("New connection created with ID:", conn_id)

def on_stream_readable(conn_id, stream_id, data):
    global Pyendpoint
    print("Server received data on connection {}, stream {}".format(conn_id, stream_id))
    response = b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\n\r\nHello, World!"
    return response
    
def socket_on_recv(Pyendpoint, py_sock):
    while True:
        try:
            data, addr = py_sock.recvfrom(4096)
            Pyendpoint.quic_endpoint_recv(data, addr)
        except BlockingIOError:
            break
        except Exception as e:
            print("Socket receive error:", e)
            break
        
def main():
    raw_socket_id = Pyendpoint.get_socket_fd()
    py_sock = socket.fromfd(raw_socket_id, socket.AF_INET, socket.SOCK_DGRAM)
    print("Socket created from file descriptor:", py_sock.getsockname())
    sel = selectors.DefaultSelector()
    sel.register(py_sock, selectors.EVENT_READ, socket_on_recv)

    while True:
        Pyendpoint.quic_process_connections()
        events = sel.select(Pyendpoint.get_quic_timeout().total_seconds())
        for key, _ in events:
            callback = key.data
            callback(Pyendpoint, py_sock)
        Pyendpoint.quic_on_timeout()

if __name__ == "__main__":
    pycallback = tquic_py.PyQuicCallBacks()
    pycallback.set_on_stream_readable_callback(on_stream_readable)
    pycallback.set_on_conn_created_callback(on_conn_created)
    Pyendpoint = tquic_py.quic_server_create("0.0.0.0:8443", pycallback)
    main()