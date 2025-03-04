use crate::behavior::Pbft;
use crate::client_handler::ClientHandler;
use crate::network_behaviour_composer::NetworkBehaviourComposer;
use crate::node_type::NodeType;
//use futures::stream::Stream;
use futures::stream::StreamExt;
use libp2p::identity::Keypair;

use libp2p::*;
use syn::Expr::Async;
use tokio;

use std::collections::VecDeque;
use std::sync::{Arc, RwLock};

mod behavior;
mod client_handler;
mod handler;
mod message;
mod network_behaviour_composer;
mod node_type;
mod protocol_config;
mod state;
mod view;

fn main() {
    println!("Hello, PBFT!");
    let cli_args: Vec<String> = std::env::args().collect();
    println!("[main] cli_args: {:?}", cli_args);
    let node_type = determine_node_type(&cli_args).expect("Usage: $ pbft [primary]");
    println!("[main] node_type: {:?}", node_type);

    let client_requests = Arc::new(RwLock::new(VecDeque::new()));
    let client_replies = Arc::new(RwLock::new(VecDeque::new()));

    let mut client_request_handler =
        ClientHandler::new(node_type, client_requests.clone(), client_replies.clone());

    let local_key = Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(local_key.public());

    let transport = development_transport(local_key.clone());
    let mut swarm = Swarm::new(
        transport,
        NetworkBehaviourComposer::new(
            libp2p::mdns::Mdns::new.expect("Failed to create mDNS service"),
            Pbft::new(local_key, client_replies.clone()),
        ),
        local_peer_id,
    );

    Swarm::listen_on(&mut swarm, "/ip4/127.0.0.1/tcp/0".parse().unwrap()).unwrap();

    let mut listening = false;

    //async fn main() -> Result<(), Box<dyn std::error::Error>> {
    //if let Some(client_request) = ||{
    //client_requests.write().unwrap().pop_front();
    // swarm.pbft.add_client_request(client_request);
    //}

    //}

    client_request_handler.tick();

    match swarm.poll().expect("Error while polling swarm") {
        syn::token::Async::Ready(Some(_)) => {}
        syn::token::Async::Ready(None) | syn::token::Async::NotReady => {
            if !listening {
                if let Some(a) = Swarm::listeners(&swarm).next() {
                    println!("Listening on {:?}", a);
                    listening = true;
                }
            }
            return Ok(syn::token::Async::NotReady);
        }
    }
}

fn determine_node_type(args: &Vec<String>) -> Result<NodeType, ()> {
    match args.len() {
        1 => Ok(NodeType::Backup),
        2 => {
            if let Some(node_type) = args.get(1) {
                if node_type == "primary" {
                    return Ok(NodeType::Primary);
                } else {
                    panic!(
                        "[main::determine_node_type] Invalid node_type: {:?}",
                        node_type
                    );
                }
            }
            {
                unreachable!();
            }
        }
        _ => Err(()),
    }
}
