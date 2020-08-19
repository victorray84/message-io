use super::common::{Message};

use message_io::events::{EventQueue};
use message_io::network::{NetworkManager, NetEvent, Endpoint};

use std::net::{SocketAddr};
use std::collections::{HashMap};

enum Event {
    Network(NetEvent<Message>),
}

pub struct Participant {
    event_queue: EventQueue<Event>,
    network: NetworkManager,
    name: String,
    discovery_endpoint: Endpoint,
    public_addr: SocketAddr,
    listened_participants: HashMap<String, Endpoint>, // Used only for free resources later
    talked_participants: HashMap<String, Endpoint>, // Used only for free resources later
}

impl Participant {
    pub fn new(name: &str) -> Option<Participant> {
        let mut event_queue = EventQueue::new();

        let network_sender = event_queue.sender().clone();
        let mut network = NetworkManager::new(move |net_event| network_sender.send(Event::Network(net_event)));

        // A listener for any other participant that want to establish connection.
        let listen_addr = "127.0.0.1:0";
        if let Ok((_, addr)) = network.listen_udp(listen_addr) {
            // Connection to the discovery server.
            let discovery_addr = "127.0.0.1:5000";
            if let Ok(endpoint) = network.connect_tcp(discovery_addr) {
                Some(Participant {
                    event_queue,
                    network,
                    name: name.to_string(),
                    discovery_endpoint: endpoint,
                    public_addr: addr, // addr has the port that listen_addr has not.
                    listened_participants: HashMap::new(),
                    talked_participants: HashMap::new(),
                })
            }
            else {
                println!("Can not connect to the discovery server at {}", discovery_addr);
                None
            }
        }
        else {
            println!("Can not listen on {}", listen_addr);
            None
        }

    }

    pub fn run(mut self) {
        // Register this participant into the discovery server
        self.network.send(self.discovery_endpoint, Message::RegisterParticipant(self.name.clone(), self.public_addr)).unwrap();

        loop {
            match self.event_queue.receive() { // Waiting events
                Event::Network(net_event) => match net_event {
                    NetEvent::Message(endpoint, message) => match message {
                        Message::ParticipantList(participants) => {
                            println!("Participant list received ({} participants)", participants.len());
                            for (name, addr) in participants {
                                self.discovered_participant(&name, addr, "I see you in the participant list");
                            }
                        },
                        Message::ParticipantNotificationAdded(name, addr) => {
                            println!("New participant '{}' in the network", name);
                            self.discovered_participant(&name, addr, "welcome to the network!");
                        },
                        Message::ParticipantNotificationRemoved(name) => {
                            println!("Removed participant '{}' from the network", name);

                            // Free related network resources to the endpoint.
                            // It is only necessary because the connections among participants are done by UDP,
                            // UDP is not connection-oriented protocol, and the AddedEndpoint / RemoveEndpoint events are not generated by UDP.
                            if let Some(endpoint) = self.listened_participants.get(&name) {
                                self.network.remove_endpoint(*endpoint).unwrap();
                            }
                            if let Some(endpoint) = self.talked_participants.get(&name) {
                                self.network.remove_endpoint(*endpoint).unwrap();
                            }
                        },
                        Message::Gretings(name, gretings) => {
                            println!("'{}' says: {}", name, gretings);
                            self.listened_participants.insert(name, endpoint);
                        }
                        _ => unreachable!(),
                    },
                    NetEvent::AddedEndpoint(_, _) => (),
                    NetEvent::RemovedEndpoint(endpoint) => {
                        if endpoint == self.discovery_endpoint {
                            return println!("Discovery server disconnected, closing");
                        }
                    },
                }
            }
        }
    }

    fn discovered_participant(&mut self, name: &str, addr: SocketAddr, message: &str) {
        if let Ok(endpoint) = self.network.connect_udp(addr) {
            let gretings = format!("Hi '{}', {}", name, message);
            self.network.send(endpoint, Message::Gretings(self.name.clone(), gretings)).unwrap();
            self.talked_participants.insert(name.to_string(), endpoint);
        }
    }
}
