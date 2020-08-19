use mio::net::{TcpListener, TcpStream, UdpSocket};
use mio::{event, Poll, Interest, Token, Events, Registry};

use std::net::{SocketAddr, SocketAddrV4, Ipv4Addr, TcpStream as StdTcpStream};
use net2::{UdpBuilder};
use std::sync::{Arc, Mutex};
use std::time::{Duration};
use std::collections::{HashMap};
use std::io::{self, prelude::*, ErrorKind};

const EVENTS_SIZE: usize = 1024;

/// Information to identify the remote endpoint.
/// The endpoint is used mainly as a connection identified.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Endpoint {
    resource_id: usize,
    addr: SocketAddr,
}

impl Endpoint {
    fn new(resource_id: usize, addr: SocketAddr) -> Endpoint {
        Endpoint { resource_id, addr }
    }

    /// Returns the connection id of the endpoint.
    /// The connection id represents the inner network resource used for this endpoint.
    /// It not must to be unique for each endpoint if some of them shared the resource.
    pub fn resource_id(&self) -> usize {
        self.resource_id
    }

    // Returns the remote address of the endpoint
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl std::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "[{}]-{}", self.resource_id, self.addr)
    }
}

#[derive(Debug)]
pub enum Event<'a> {
    Connection,
    Data(&'a[u8]),
    Disconnection,
}

pub enum Listener {
    Tcp(TcpListener),
    Udp(UdpSocket),
}

impl Listener {
    pub fn new_tcp(addr: SocketAddr) -> io::Result<Listener> {
        TcpListener::bind(addr).map(|listener| Listener::Tcp(listener))
    }

    pub fn new_udp(addr: SocketAddr) -> io::Result<Listener> {
        UdpSocket::bind(addr).map(|socket| Listener::Udp(socket))
    }

    pub fn new_udp_multicast(addr: SocketAddrV4) -> io::Result<Listener> {
        let listening_addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, addr.port());
        UdpBuilder::new_v4().unwrap().reuse_address(true).unwrap().bind(listening_addr).map(|socket| {
            socket.join_multicast_v4(&addr.ip(), &Ipv4Addr::UNSPECIFIED).unwrap();
            Listener::Udp(UdpSocket::from_std(socket))
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        match self {
            Listener::Tcp(listener) => listener.local_addr().unwrap(),
            Listener::Udp(socket) => socket.local_addr().unwrap(),
        }
    }

    pub fn event_source(&mut self) -> &mut dyn event::Source {
        match self {
            Listener::Tcp(listener) => listener,
            Listener::Udp(socket) => socket,
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        match self {
            Listener::Udp(socket) => {
                if let SocketAddr::V4(addr) = socket.local_addr().unwrap() {
                    if addr.ip().is_multicast() {
                        socket.leave_multicast_v4(&addr.ip(), &Ipv4Addr::UNSPECIFIED).unwrap();
                    }
                }
            },
            _ => (),
        }
    }
}

pub enum Remote {
    Tcp(TcpStream),
    Udp(UdpSocket, SocketAddr),
}

impl Remote {
    pub fn new_tcp(addr: SocketAddr) -> io::Result<Remote> {
        // Create a standard TcpStream to blocking until the connection is reached.
        StdTcpStream::connect(addr).map(|stream| {
            stream.set_nonblocking(true).unwrap();
            Remote::Tcp(TcpStream::from_std(stream))
        })
    }

    pub fn new_udp(addr: SocketAddr) -> io::Result<Remote> {
        UdpSocket::bind("0.0.0.0:0".parse().unwrap()).map(|socket| {
            socket.connect(addr).unwrap();
            Remote::Udp(socket, addr)
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        match self {
            Remote::Tcp(stream) => stream.local_addr().unwrap(),
            Remote::Udp(socket, _) => socket.local_addr().unwrap(),
        }
    }

    pub fn peer_addr(&self) -> SocketAddr {
        match self {
            Remote::Tcp(stream) => stream.peer_addr().unwrap(),
            Remote::Udp(_, addr) => *addr,
        }
    }

    pub fn event_source(&mut self) -> &mut dyn event::Source {
        match self {
            Remote::Tcp(stream) => stream,
            Remote::Udp(socket, _) => socket,
        }
    }
}

enum Resource {
    Listener(Listener),
    Remote(Remote),
}

impl Resource {
    pub fn event_source(&mut self) -> &mut dyn event::Source {
        match self {
            Resource::Listener(listener) => listener.event_source(),
            Resource::Remote(remote) => remote.event_source(),
        }
    }

    pub fn local_addr(&self) -> SocketAddr {
        match self {
            Resource::Listener(listener) => listener.local_addr(),
            Resource::Remote(remote) => remote.local_addr(),
        }
    }
}

impl std::fmt::Display for Resource {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let resource = match self {
            Resource::Listener(listener) => match listener {
                Listener::Tcp(_) => "Listener::Tcp",
                Listener::Udp(_) => "Listener::Udp",
            },
            Resource::Remote(remote) => match remote {
                Remote::Tcp(_) => "Remote::Tcp",
                Remote::Udp(_, _) => "Remote::Udp",
            },
        };
        write!(f, "{}", resource)
    }
}

pub fn adapter() -> (Arc<Mutex<Controller>>, Receiver) {
    let poll = Poll::new().unwrap();
    let controller = Controller::new(poll.registry().try_clone().unwrap());
    let thread_safe_controller = Arc::new(Mutex::new(controller));
    (thread_safe_controller.clone(), Receiver::new(thread_safe_controller, poll))
}

pub struct Controller {
    resources: HashMap<usize, Resource>,
    last_id: usize,
    registry: Registry,
}

impl Controller {
    fn new(registry: Registry) -> Controller {
        Controller {
            resources: HashMap::new(),
            last_id: 0,
            registry,
        }
    }

    fn add_resource<S: event::Source + ?Sized>(&mut self, source: &mut S) -> usize {
        let id = self.last_id;
        self.last_id += 1;
        self.registry.register(source, Token(id), Interest::READABLE).unwrap();
        id
    }

    pub fn add_remote(&mut self, mut remote: Remote) -> Endpoint {
        let id = self.add_resource(remote.event_source());
        let endpoint = Endpoint::new(id, remote.peer_addr());
        self.resources.insert(id, Resource::Remote(remote));
        endpoint
    }

    pub fn add_listener(&mut self, mut listener: Listener) -> (usize, SocketAddr) {
        let id = self.add_resource(listener.event_source());
        let local_addr = listener.local_addr();
        self.resources.insert(id, Resource::Listener(listener));
        (id, local_addr)
    }

    pub fn remove_resource(&mut self, resource_id: usize) -> Option<()> {
        if let Some(mut resource) = self.resources.remove(&resource_id) {
            self.registry.deregister(resource.event_source()).unwrap();
            Some(())
        }
        else { None }
    }

    pub fn local_address(&mut self, resource_id: usize) -> Option<SocketAddr> {
        if let Some(resource) = self.resources.get(&resource_id) {
            Some(resource.local_addr())
        }
        else { None }
    }

    pub fn send(&mut self, endpoint: Endpoint, data: &[u8]) -> io::Result<()> {
        if let Some(resource) = self.resources.get_mut(&endpoint.resource_id()) {
            match resource {
                Resource::Listener(listener) => match listener {
                    Listener::Udp(socket) => socket.send_to(data, endpoint.addr()).map(|_|()),
                    _ => unreachable!(),
                },
                Resource::Remote(remote) => match remote {
                    Remote::Tcp(stream) => stream.write(data).map(|_|()),
                    Remote::Udp(socket, _) => socket.send(data).map(|_|()),
                },
            }
        }
        else {
            Err(io::Error::new(ErrorKind::NotFound, format!("Resource id '{}' not exists in the network adapter", endpoint.resource_id())))
        }
    }
}

pub struct Receiver {
    controller: Arc<Mutex<Controller>>,
    poll: Poll,
    events: Events,
}

impl<'a> Receiver {
    fn new(controller: Arc<Mutex<Controller>>, poll: Poll) -> Receiver {
        Receiver {
            controller,
            poll,
            events: Events::with_capacity(EVENTS_SIZE),
        }
    }

    pub fn receive<C>(&mut self, input_buffer: &mut[u8], timeout: Option<Duration>, callback: C)
    where C: for<'b> FnMut(Endpoint, Event<'b>) {
        loop {
            match self.poll.poll(&mut self.events, timeout) {
                Ok(_) => {
                    break self.process_event(input_buffer, callback)
                },
                Err(e) => match e.kind() {
                    ErrorKind::Interrupted => continue,
                    _ => Err(e).unwrap(),
                }
            }
        }
    }

    fn process_event<C>(&mut self, input_buffer: &mut[u8], mut callback: C)
    where C: for<'b> FnMut(Endpoint, Event<'b>) {
        for mio_event in &self.events {
            let token = mio_event.token();
            let id = token.0;
            let mut controller = self.controller.lock().unwrap();

            let resource = controller.resources.get_mut(&id).unwrap();
            log::trace!("Wake from poll for endpoint {}. Resource: {}", id, resource);
            match resource {
                Resource::Listener(listener) => match listener {
                    Listener::Tcp(listener) => {
                        let mut listener = listener;
                        loop {
                            match listener.accept() {
                                Ok((stream, _)) => {
                                    let endpoint = controller.add_remote(Remote::Tcp(stream));
                                    callback(endpoint, Event::Connection);

                                    // Used to avoid the consecutive mutable borrows
                                    listener = match controller.resources.get_mut(&id).unwrap() {
                                        Resource::Listener(Listener::Tcp(listener)) => listener,
                                        _ => unreachable!(),
                                    }
                                }
                                Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => break,
                                Err(ref err) if err.kind() == io::ErrorKind::Interrupted => continue,
                                Err(err) => Err(err).unwrap(),
                            }
                        }
                    },
                    Listener::Udp(socket) => {
                        loop {
                            match socket.recv_from(input_buffer) {
                                Ok((_, addr)) => callback(Endpoint::new(id, addr), Event::Data(input_buffer)),
                                Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => break,
                                Err(err) => Err(err).unwrap(),
                            }
                        }
                    },
                }
                Resource::Remote(remote) => match remote {
                    Remote::Tcp(stream) => {
                        loop {
                            match stream.read(input_buffer) {
                                Ok(0) => {
                                    let endpoint = Endpoint::new(id, stream.peer_addr().unwrap());
                                    controller.remove_resource(endpoint.resource_id()).unwrap();
                                    callback(endpoint, Event::Disconnection);
                                    break;
                                },
                                Ok(_) => {
                                    let endpoint = Endpoint::new(id, stream.peer_addr().unwrap());
                                    callback(endpoint, Event::Data(input_buffer));
                                },
                                Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => break,
                                Err(ref err) if err.kind() == io::ErrorKind::Interrupted => continue,
                                Err(err) => Err(err).unwrap(),
                            }
                        }
                    },
                    Remote::Udp(socket, addr) => {
                        loop {
                            match socket.recv(input_buffer) {
                                Ok(_) => callback(Endpoint::new(id, *addr), Event::Data(input_buffer)),
                                Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => break,
                                Err(ref err) if err.kind() == io::ErrorKind::ConnectionRefused => continue,
                                Err(err) => Err(err).unwrap(),
                            }
                        }
                    },
                },
            }
        };
    }
}

