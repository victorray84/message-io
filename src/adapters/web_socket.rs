use crate::adapter::{Adapter, ActionHandler, EventHandler};
use crate::status::{SendStatus, AcceptStatus, ReadStatus};
use crate::util::{OTHER_THREAD_ERR};

use mio::{
    event::{Source},
    Interest, Token, Registry,
};
use mio::net::{TcpStream, TcpListener};

use tungstenite::protocol::{WebSocket, Role, Message};
use tungstenite::server::{accept as ws_accept};
use tungstenite::error::{Error};

use std::sync::{Mutex};
use std::net::{SocketAddr, TcpStream as StdTcpStream};
use std::io::{self, ErrorKind};

pub struct WebSocketWrapper(Mutex<WebSocket<TcpStream>>);
impl Source for WebSocketWrapper {
    fn register(
        &mut self,
        registry: &Registry,
        token: Token,
        interests: Interest,
    ) -> io::Result<()>
    {
        self.0.lock().expect(OTHER_THREAD_ERR).get_mut().register(registry, token, interests)
    }

    fn reregister(
        &mut self,
        registry: &Registry,
        token: Token,
        interests: Interest,
    ) -> io::Result<()>
    {
        self.0.lock().expect(OTHER_THREAD_ERR).get_mut().reregister(registry, token, interests)
    }

    fn deregister(&mut self, registry: &Registry) -> io::Result<()> {
        self.0.lock().expect(OTHER_THREAD_ERR).get_mut().deregister(registry)
    }
}

impl WebSocketWrapper {
    pub fn new(web_socket: WebSocket<TcpStream>) -> Self {
       Self(Mutex::new(web_socket))
    }
}

pub struct WsAdapter;
impl Adapter for WsAdapter {
    type Remote = WebSocketWrapper;
    type Listener = TcpListener;
    type ActionHandler = WsActionHandler;
    type EventHandler = WsEventHandler;

    fn split(self) -> (WsActionHandler, WsEventHandler) {
        (WsActionHandler, WsEventHandler)
    }
}

pub struct WsActionHandler;
impl ActionHandler for WsActionHandler {
    type Remote = WebSocketWrapper;
    type Listener = TcpListener;

    fn connect(&mut self, addr: SocketAddr) -> io::Result<WebSocketWrapper> {
        let stream = StdTcpStream::connect(addr)?;
        stream.set_nonblocking(true)?;
        let stream = TcpStream::from_std(stream);
        Ok(WebSocketWrapper::new(WebSocket::from_raw_socket(stream, Role::Client, None)))
    }

    fn listen(&mut self, addr: SocketAddr) -> io::Result<(TcpListener, SocketAddr)> {
        let listener = TcpListener::bind(addr)?;
        let real_addr = listener.local_addr().unwrap();
        Ok((listener, real_addr))
    }

    fn send(&mut self, web_socket: &WebSocketWrapper, data: &[u8]) -> SendStatus {
        let message = Message::Binary(data.to_vec());
        let mut socket = web_socket.0.lock().expect(OTHER_THREAD_ERR);
        let mut result = socket.write_message(message);
        loop {
            match result {
                Ok(_) => break SendStatus::Sent,
                Err(Error::Io(ref err)) if err.kind() == ErrorKind::WouldBlock  => {
                    result = socket.write_pending();
                }
                Err(_) => break SendStatus::ResourceNotFound,
            }
        }
    }
}

pub struct WsEventHandler;
impl EventHandler for WsEventHandler {
    type Remote = WebSocketWrapper;
    type Listener = TcpListener;

    fn accept_event(&mut self, listener: &TcpListener) -> AcceptStatus<'_, WebSocketWrapper> {
        match listener.accept() {
            Ok((stream, addr)) => {
                let ws_socket = WebSocketWrapper::new(ws_accept(stream).unwrap());
                AcceptStatus::AcceptedRemote(addr, ws_socket)
            }
            Err(ref err) if err.kind() == ErrorKind::WouldBlock => AcceptStatus::WaitNextEvent,
            Err(ref err) if err.kind() == ErrorKind::Interrupted => AcceptStatus::Interrupted,
            Err(_) => {
                log::trace!("WebSocket process listener error");
                AcceptStatus::WaitNextEvent // Should not happen
            }
        }
    }

    fn read_event(
        &mut self,
        web_socket: &WebSocketWrapper,
        process_data: &dyn Fn(&[u8]),
    ) -> ReadStatus
    {
        match web_socket.0.lock().expect(OTHER_THREAD_ERR).read_message() {
            Ok(message) => match message {
                Message::Binary(data) => {
                    process_data(&data);
                    ReadStatus::MoreData
                }
                Message::Close(_) => ReadStatus::Disconnected,
                _ => ReadStatus::MoreData,
            }
            Err(Error::Io(ref err)) if err.kind() == ErrorKind::WouldBlock
                => ReadStatus::WaitNextEvent,
            Err(_) => ReadStatus::Disconnected, // should not happen
        }
    }
}
