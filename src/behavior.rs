use crate::handler::{PbftHandler, PbftHandlerEvent, PbftHandlerIn};
use crate::message::{ClientReply, ClientRequest, Commit, PrePrepare, PrePrepareSequence, Prepare};
use crate::state::State;
use libp2p::core::ConnectedPoint;
use libp2p::identity::Keypair;
use libp2p::multiaddr::Multiaddr;
use libp2p::swarm::{NetworkBehaviour, NetworkBehaviourAction, PollParameters};
use libp2p::PeerId;
use std::collections::{HashMap, HashSet, VecDeque};
use std::error::Error;
use std::sync::{Arc, RwLock};
use tokio::prelude::{Async, AsyncRead, AsyncWrite};

pub struct Pbft<TSubstream> {
    keypair: Keypair,
    addresses: HashMap<PeerId, HashSet<Multiaddr>>,
    connected_peers: HashSet<PeerId>,
    queued_events: VecDeque<NetworkBehaviourAction<PbftHandlerIn, PbftEvent>>,
    state: State,
    pre_prepare_sequence: PrePrepareSequence,
    client_replies: Arc<RwLock<VecDeque<ClientReply>>>,
    _marker: std::marker::PhantomData<TSubstream>,
}

impl<TSubstream> Pbft<TSubstream> {
    pub fn new(keypair: Keypair, client_replies: Arc<RwLock<VecDeque<ClientReply>>>) -> Self {
        Self {
            keypair,
            addresses: HashMap::new(),
            connected_peers: HashSet::new(),
            queued_events: VecDeque::with_capacity(100), // FIXME
            state: State::new(),
            pre_prepare_sequence: PrePrepareSequence::new(),
            client_replies,
            _marker: std::marker::PhantomData,
        }
    }

    pub fn has_peer(&self, peer_id: &PeerId) -> bool {
        self.connected_peers
            .iter()
            .any(|connected_peer_id| connected_peer_id.clone() == peer_id.clone())
    }

    pub fn add_peer(&mut self, peer_id: &PeerId, address: &Multiaddr) {
        println!("[Pbft::add_peer] {:?}, {:?}", peer_id, address);
        {
            let mut addresses = match self.addresses.get(peer_id) {
                Some(addresses) => addresses.clone(),
                None => HashSet::new(),
            };
            addresses.insert(address.clone());

            self.addresses.insert(peer_id.clone(), addresses.clone());
        }

        self.queued_events
            .push_back(NetworkBehaviourAction::DialPeer {
                peer_id: peer_id.clone(),
            });
    }

    pub fn add_client_request(&mut self, client_request: ClientRequest) {
        println!(
            "[Pbft::add_client_request] client_request: {:?}",
            client_request
        );

        // In the pre-prepare phase, the primary assigns a sequence number, n, to the request
        self.pre_prepare_sequence.increment();
        let pre_prepare = PrePrepare::from(
            self.state.current_view(),
            self.pre_prepare_sequence.value(),
            client_request,
        );

        println!(
            "[Pbft::add_client_request] [broadcasting the pre_prepare message] pre_prepare: {:?}",
            pre_prepare
        );
        println!(
            "[Pbft::add_client_request] [broadcasting to the peers] connected_peers: {:?}",
            self.connected_peers
        );
        if self.connected_peers.is_empty() {
            panic!("[Pbft::add_client_request] !!! connected_peers is empty !!!");
        }

        for peer_id in self.connected_peers.iter() {
            self.queued_events
                .push_back(NetworkBehaviourAction::SendEvent {
                    peer_id: peer_id.clone(),
                    event: PbftHandlerIn::PrePrepareRequest(pre_prepare.clone()),
                });
        }

        self.process_pre_prepare(pre_prepare).unwrap(); // TODO: error handling
    }

    fn process_pre_prepare(&mut self, pre_prepare: PrePrepare) -> Result<(), String> {
        self.validate_pre_prepare(&pre_prepare)?;
        self.state.insert_pre_prepare(pre_prepare.clone());

        // If backup replica accepts the message, it enters the prepare phase by multicasting a PREPARE message to
        // all other replicas and adds both messages to its log.
        let prepare = Prepare::from(&pre_prepare);
        self.state.insert_prepare(
            PeerId::from_public_key(self.keypair.public()),
            prepare.clone(),
        );

        if self.connected_peers.is_empty() {
            panic!("[Pbft::process_pre_prepare] !!! Peers not found !!!");
        }

        for peer_id in self.connected_peers.iter() {
            self.queued_events
                .push_back(NetworkBehaviourAction::SendEvent {
                    peer_id: peer_id.clone(),
                    event: PbftHandlerIn::PrepareRequest(prepare.clone()),
                })
        }
        Ok(())
    }

    fn validate_pre_prepare(&self, pre_prepare: &PrePrepare) -> Result<(), String> {
        // TODO: the signatures in the request and the pre-prepare message are correct

        // _d_ is the digest for _m_
        pre_prepare.validate_digest()?;

        {
            // it is in view _v_
            let current_view = self.state.current_view();
            if pre_prepare.view() != current_view {
                return Err(format!(
                    "view number isn't matched. message: {}, state: {}",
                    pre_prepare.view(),
                    current_view
                ));
            }

            // it has not accepted a pre-prepare message for view _v_ and sequence number _n_ containing a different digest
            match self.state.get_pre_prepare(pre_prepare) {
                Some(stored_pre_prepare) => {
                    if pre_prepare.digest() != stored_pre_prepare.digest() {
                        return Err(format!("The pre-prepare key has already stored into logs and its digest dont match. message: {}, stored message: {}", pre_prepare, stored_pre_prepare));
                    }
                }
                None => {}
            }
        }

        // TODO: the sequence number in the pre-prepare message is between a low water mark, _h_, and a high water mark, _H_

        Ok(())
    }

    fn validate_prepare(&self, prepare: &Prepare) -> Result<(), String> {
        // The replicas verify whether the prepares match the pre-prepare by checking that they have the
        // same view, sequence number, and digest.
        if let Some(pre_prepare) = self
            .state
            .get_pre_prepare_by_key(prepare.view(), prepare.sequence_number())
        {
            if pre_prepare.digest() == prepare.digest() {
                return Ok(());
            }
            return Err(format!("the Prepare request doesn't match with the PrePrepare. prepare: {}, pre-prepare: {}", prepare, pre_prepare));
        }
        Err(format!(
            "No PrePrepare that matches with the Prepare. prepare: {}",
            prepare
        ))
    }

    fn prepared(&self, view: u64, sequence_number: u64) -> bool {
        // 2f prepares from different backups that match the pre-prepare.
        let len = self.state.prepare_len(view, sequence_number);
        println!("[Pbft::prepared] prepare_len: {}", len);
        len >= 1 // TODO
    }

    fn validate_commit(&self, commit: &Commit) -> Result<(), String> {
        // TODO: properly signed

        // the view number in the message is equal to the replica's current view
        if commit.view() != self.state.current_view() {
            return Err(format!("The view number in the message is NOT equal to the replica's current view. Commit.view: {}, current_view: {}", commit.view(), self.state.current_view()));
        }

        // TODO: the sequence number is between h and H

        Ok(())
    }

    // `committed(m, v, n)` is true if and only if `prepared(m, v, n, i)` is true for all _i_ in
    // some set of `f + 1` non-faulty replicas.
    #[allow(dead_code)]
    fn committed(&self, view: u64, sequence_number: u64) -> bool {
        let len = self.state.commit_len(view);
        let prepared = self.prepared(view, sequence_number);

        println!(
            "[Pbft::committed] commit_len: {}, prepared: {}",
            len, prepared
        );
        prepared && len >= 1 // TODO: f + 1
    }

    // `committed-local(m, v, n, i)` is true if and only if `prepared(m, v, n, i)` is true and _i_
    // has accepted `2f + 1` commits (possibly including its own) from different replicas that match
    // the pre-prepare for _m_.
    fn committed_local(&self, view: u64, sequence_number: u64) -> bool {
        let len = self.state.commit_len(view);
        let prepared = self.prepared(view, sequence_number);

        println!(
            "[Pbft::committed_local] commit_len: {}, prepared: {}",
            len, prepared
        );
        prepared && len >= 1 // TODO: 2f + 1
    }
}

#[derive(Debug)]
pub struct PbftFailure;
impl Error for PbftFailure {}

impl std::fmt::Display for PbftFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("pbft failure")
    }
}

#[derive(Debug)]
pub struct PbftEvent;

impl<TSubstream> NetworkBehaviour for Pbft<TSubstream>
where
    TSubstream: AsyncRead + AsyncWrite,
{
    type ProtocolsHandler = PbftHandler<TSubstream>;
    type OutEvent = PbftEvent;

    fn new_handler(&mut self) -> Self::ProtocolsHandler {
        println!("Pbft::new_handler()");
        PbftHandler::new()
    }

    fn addresses_of_peer(&mut self, peer_id: &PeerId) -> Vec<Multiaddr> {
        println!("[Pbft::addresses_of_peer] peer_id: {:?}", peer_id);
        match self.addresses.get(peer_id) {
            Some(addresses) => {
                println!(
                    "[Pbft::addresses_of_peer] peer_id: {:?}, addresses: {:?}",
                    peer_id, addresses
                );
                addresses.clone().into_iter().collect()
            }
            None => {
                println!(
                    "[Pbft::addresses_of_peer] addresses not found. peer_id: {:?}",
                    peer_id
                );
                Vec::new()
            }
        }
    }

    fn inject_connected(&mut self, peer_id: PeerId, connected_point: ConnectedPoint) {
        println!(
            "[Pbft::inject_connected] peer_id: {:?}, connected_point: {:?}",
            peer_id, connected_point
        );
        //        match connected_point {
        //            ConnectedPoint::Dialer { address } => {
        //            },
        //            ConnectedPoint::Listener { .. } => {}
        //        };
        self.connected_peers.insert(peer_id);
        println!(
            "[Pbft::inject_connected] connected_peers: {:?}, addresses: {:?}",
            self.connected_peers, self.addresses
        );
    }

    fn inject_disconnected(&mut self, peer_id: &PeerId, connected_point: ConnectedPoint) {
        println!(
            "[Pbft::inject_disconnected] {:?}, {:?}",
            peer_id, connected_point
        );
        //        let address = match connected_point {
        //            ConnectedPoint::Dialer { address } => address,
        //            ConnectedPoint::Listener { local_addr: _, send_back_addr } => send_back_addr
        //        };
        self.connected_peers.remove(peer_id);
        println!(
            "[Pbft::inject_disconnected] connected_peers: {:?}, addresses: {:?}",
            self.connected_peers, self.addresses
        );
    }

    fn inject_node_event(&mut self, peer_id: PeerId, handler_event: PbftHandlerEvent) {
        println!(
            "[Pbft::inject_node_event] handler_event: {:?}",
            handler_event
        );
        match handler_event {
            PbftHandlerEvent::ProcessPrePrepareRequest {
                request,
                connection_id,
            } => {
                println!(
                    "[Pbft::inject_node_event] [PbftHandlerEvent::PrePrepareRequest] request: {:?}",
                    request
                );
                self.process_pre_prepare(request.clone()).unwrap(); // TODO: error handling

                self.queued_events
                    .push_back(NetworkBehaviourAction::SendEvent {
                        peer_id,
                        event: PbftHandlerIn::PrePrepareResponse("OK".into(), connection_id),
                    });
            }
            PbftHandlerEvent::Response { response } => {
                let response_message =
                    String::from_utf8(response).expect("Failed to parse response");
                println!(
                    "[Pbft::inject_node_event] [PbftHandlerEvent::Response] response_message: {:?}",
                    response_message
                );
                if response_message == "OK" {
                    println!("[Pbft::inject_node_event] [PbftHandlerEvent::Response] the communications has done successfully")
                } else {
                    // TODO: retry?
                    eprintln!("[Pbft::inject_node_event] [PbftHandlerEvent::Response] response_message: {:?}", response_message);
                }
            }
            PbftHandlerEvent::ProcessPrepareRequest {
                request,
                connection_id,
            } => {
                println!("[Pbft::inject_node_event] [PbftHandlerEvent::ProcessPrepareRequest] request: {:?}", request);
                self.validate_prepare(&request).unwrap();
                self.state.insert_prepare(peer_id.clone(), request.clone());

                self.queued_events
                    .push_back(NetworkBehaviourAction::SendEvent {
                        peer_id,
                        event: PbftHandlerIn::PrepareResponse("OK".into(), connection_id),
                    });

                if self.prepared(request.view(), request.sequence_number()) {
                    let commit: Commit = request.into();
                    for p in self.connected_peers.iter() {
                        self.queued_events
                            .push_back(NetworkBehaviourAction::SendEvent {
                                peer_id: p.clone(),
                                event: PbftHandlerIn::CommitRequest(commit.clone()),
                            })
                    }
                }
            }
            PbftHandlerEvent::ProcessCommitRequest {
                request,
                connection_id,
            } => {
                println!("[Pbft::inject_node_event] [PbftHandlerEvent::ProcessCommitRequest] request: {:?}", request);

                self.validate_commit(&request).unwrap();

                self.queued_events
                    .push_back(NetworkBehaviourAction::SendEvent {
                        peer_id: peer_id.clone(),
                        event: PbftHandlerIn::CommitResponse("OK".into(), connection_id),
                    });

                // Replicas accept commit messages and insert them in their log
                self.state.insert_commit(peer_id, request.clone());

                // Each replica _i_ executes the operation requested by _m_ after `committed-local(m, v, n, i)` is true
                if self.committed_local(request.view(), request.sequence_number()) {
                    let client_request = self
                        .state
                        .get_pre_prepare_by_key(request.view(), request.sequence_number())
                        .unwrap()
                        .client_reqeust();
                    println!("[Pbft::inject_node_event] [PbftHandlerEvent::ProcessCommitRequest] client_message: {:?}", client_request);

                    // Discard requests whose timestamp is lower than the timestamp in the last reply this node sent to the client to guarantee exactly-once semantics.
                    if client_request.timestamp() <= self.state.last_timestamp() {
                        eprintln!(
                            "[Pbft::inject_node_event] [PbftHandlerEvent::ProcessCommitRequest] the request was discarded as its timestamp is lower than the last timestamp. last_timestamp: {:?}",
                            self.state.last_timestamp()
                        );
                        return;
                    }

                    println!("[Pbft::inject_node_event] [PbftHandlerEvent::ProcessCommitRequest] the operation has been executed: {:?}", client_request.operation());

                    // After executing the requested operation, replicas send a reply to the client.
                    let reply = ClientReply::new(
                        PeerId::from_public_key(self.keypair.public()),
                        client_request,
                        &request,
                    );
                    println!("[Pbft::inject_node_event] [PbftHandlerEvent::ProcessCommitRequest] reply: {:?}", reply);
                    self.state.update_last_timestamp(reply.timestamp());
                    self.client_replies.write().unwrap().push_back(reply);
                }
            }
        }
    }

    fn poll(
        &mut self,
        _: &mut impl PollParameters,
    ) -> Async<NetworkBehaviourAction<PbftHandlerIn, PbftEvent>> {
        println!("[Pbft::poll]");
        if let Some(event) = self.queued_events.pop_front() {
            println!("[Pbft::poll] event: {:?}", event);
            return Async::Ready(event);
        }
        Async::NotReady
    }
}
