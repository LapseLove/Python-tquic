import tquic_py
import socket
import selectors

flag = True
def process_cmd(Pyendpoint):
    global flag
    if flag:
            w = Pyendpoint.send(b"GET /api\r\n")
            if w > 0:
                print("Application sent data: GET \\r\\n")
                flag = False
    if Pyendpoint.check_recv_queue() != 0:
        data = Pyendpoint.recv()
        print("Application received data: {}".format(data))
        Pyendpoint.quic_connection_close()
        Pyendpoint.quic_process_connections()
        return True
    return False


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
    Pyendpoint = tquic_py.quic_client_create()
    Pyendpoint.connect("127.0.0.1:8443")
    raw_socket_id = Pyendpoint.get_socket_fd()
    py_sock = socket.fromfd(raw_socket_id, socket.AF_INET, socket.SOCK_DGRAM)
    print("Socket created from file descriptor:", py_sock.getsockname())
    sel = selectors.DefaultSelector()
    sel.register(py_sock, selectors.EVENT_READ, socket_on_recv)

    while True:
        Pyendpoint.quic_process_connections()
        if process_cmd(Pyendpoint):
            break
        events = sel.select(Pyendpoint.get_quic_timeout().total_seconds())
        for key, _ in events:
            callback = key.data
            callback(Pyendpoint, py_sock)
        Pyendpoint.quic_on_timeout()

if __name__ == "__main__":
    main()