import tquic_py
import socket
import selectors
import time
import os

def on_conn_created(conn_id):
    print("New connection created with ID:", conn_id)

def on_stream_readable(conn_id, stream_id, data):
    global Pyendpoint, response_map
    # print("Client received data on connection {}, stream {}: data: {}".format(conn_id, stream_id, data))
    response_map.pop(stream_id, None)
    return None

def process_cmd(Pyendpoint):
    global flag, response_map, stream_id, init
    if flag < 10:
        b = Pyendpoint.get_sqp_bandwidth_estimate()
        data_size = b / 30 / 8
        payload = os.urandom(int(data_size))
        w = Pyendpoint.send_sqp(payload)
        if w > 0:
            # print("write size: {} bytes".format(w))
            # print("Application sent data of size: {} bytes".format(len(payload)))
            if init == False:
                init = True
            flag += 1
            response_map[stream_id] = 1
            stream_id += 4

def check_responses_empty(Pyendpoint):
    global response_map
    if not response_map and init == True:
        print("All responses received.")
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
    raw_socket_id = Pyendpoint.get_socket_fd()
    py_sock = socket.fromfd(raw_socket_id, socket.AF_INET, socket.SOCK_DGRAM)
    print("Socket created from file descriptor:", py_sock.getsockname())
    sel = selectors.DefaultSelector()
    sel.register(py_sock, selectors.EVENT_READ, socket_on_recv)
    while True:
        Pyendpoint.quic_process_connections()
        process_cmd(Pyendpoint)
        if check_responses_empty(Pyendpoint):
            break
        events = sel.select(Pyendpoint.get_quic_timeout().total_seconds())
        for key, _ in events:
            callback = key.data
            callback(Pyendpoint, py_sock)
        Pyendpoint.quic_on_timeout()

if __name__ == "__main__":
    SERVER_IP = os.environ.get('MAHIMAHI_BASE', '127.0.0.1')
    pycallback = tquic_py.PyQuicCallBacks()
    pycallback.set_on_stream_readable_callback(on_stream_readable)
    pycallback.set_on_conn_created_callback(on_conn_created)
    Pyendpoint = tquic_py.quic_client_create(pycallback)
    Pyendpoint.connect(f"{SERVER_IP}:8443")
    print("Connecting to server at {}:8443".format(SERVER_IP))
    response_map = {}
    flag = 0
    stream_id = 0
    init = False
    main()