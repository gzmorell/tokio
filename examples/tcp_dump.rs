//! A chat server that broadcasts a message to all connections.
//!
//! This example is explicitly more verbose than it has to be. This is to
//! illustrate more concepts.
//!
//! A chat server for telnet clients. After a telnet client connects, the first
//! line should contain the client's name. After that, all lines sent by a
//! client are broadcasted to all other connected clients.
//!
//! Because the client is telnet, lines are delimited by "\r\n".
//!
//! You can test this out by running:
//!
//!     cargo run --example chat
//!
//! And then in another terminal run:
//!
//!     telnet localhost 6142
//!
//! You can run the `telnet` command in any number of additional windows.
//!
//! You can run the second command in multiple windows and then chat between the
//! two, seeing the messages from the other client as they're received. For all
//! connected clients they'll all join the same room and see everyone else's
//! messages.

#![warn(rust_2018_idioms)]

use bytes::BytesMut;
use futures::SinkExt;
use std::collections::HashMap;
use std::error::Error;
use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex, broadcast};
use tokio_stream::StreamExt;
use tokio_util::codec::{BytesCodec, Framed};
use std::env;
use stubborn_io::StubbornTcpStream;
use stubborn_io::ReconnectOptions;



#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    use tracing_subscriber::{fmt::format::FmtSpan, EnvFilter};
    // Configure a `tracing` subscriber that logs traces emitted by the chat
    // server.
    tracing_subscriber::fmt()
        // Filter what traces are displayed based on the RUST_LOG environment
        // variable.
        //
        // Traces emitted by the example code will always be displayed. You
        // can set `RUST_LOG=tokio=trace` to enable additional traces emitted by
        // Tokio itself.
        .with_env_filter(EnvFilter::from_default_env().add_directive("tcp_dump=info".parse()?))
        // Log events when `tracing` spans are created, entered, exited, or
        // closed. When Tokio's internal tracing support is enabled (as
        // described above), this can be used to track the lifecycle of spawned
        // tasks on the Tokio runtime.
        .with_span_events(FmtSpan::FULL)
        // Set this subscriber as the default, to collect all traces emitted by
        // the program.
        .init();

    // Create the shared state. This is how all the peers communicate.
    //
    // The server task will hold a handle to this. For every new client, the
    // `state` handle is cloned and passed into the task that processes the
    // client connection.
    
    let addr = env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:6142".to_string());

    let server = env::args()
    .nth(2)
    .unwrap_or_else(|| "127.0.0.1:6140".to_string());

    let server_address = server.parse::<SocketAddr>()?;
    let options = ReconnectOptions::new().with_exit_if_first_connect_fails(false);
    let client_stream = StubbornTcpStream::connect_with_options(server_address, options).await?;
    tracing::info!("Connected to Server");

    let state = Arc::new(Mutex::new(Shared::new()));

    // Bind a TCP listener to the socket address.
    //
    // Note that this is the Tokio TcpListener, which is fully async.
    let listener = TcpListener::bind(&addr).await?;

    tracing::info!("server running on {}", addr);
    let st = Arc::clone(&state);

    let (stx, srx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        tracing::info!("Server connection process");
        if let Err(e) = process_server_connection(st, client_stream, server_address, srx).await {
            tracing::info!("an error occurred; error = {:?}", e);
        }
    });
    

    loop {
        
        let (stream, addr) = listener.accept().await?;
        tracing::info!("Connected to Server");
        // Asynchronously wait for an inbound TcpStream.
        // Clone a handle to the `Shared` state for the new connection.
        let state = Arc::clone(&state);
        let stx2 = stx.clone();

        // Spawn our handler to be run asynchronously.
        tokio::spawn(async move {
            tracing::info!("Accepted connection");
            if let Err(e) = process(state, stream, addr, stx2).await {
                tracing::info!("an error occurred; error = {:?}", e);
            }
        });
    }
}

/// Shorthand for the transmit half of the message channel.
type Tx = mpsc::UnboundedSender<BytesMut>;

/// Shorthand for the receive half of the message channel.
type Rx = mpsc::UnboundedReceiver<BytesMut>;



/// Data that is shared between all peers in the chat server.
///
/// This is the set of `Tx` handles for all connected clients. Whenever a
/// message is received from a client, it is broadcasted to all peers by
/// iterating over the `peers` entries and sending a copy of the message on each
/// `Tx`.
struct Shared {
    peers: HashMap<SocketAddr, Tx>,
}

impl Shared {
    /// Create a new, empty, instance of `Shared`.
    fn new() -> Self {
        Shared {
            peers: HashMap::new(),
        }
    }

    /// Send a `LineCodec` encoded message to every peer, except
    /// for the sender.
    async fn broadcast(&mut self, sender: SocketAddr, message: &BytesMut) {
        for peer in self.peers.iter_mut() {
            if *peer.0 != sender {
                let _ = peer.1.send(message.to_owned());
                println!(
                    "Peer {} command received: {:?}",
                    peer.0.to_string(),
                    message
                );
            }
        }
    }
}

struct Connection {
    /// The TCP socket wrapped with the `Lines` codec, defined below.
    ///
    /// This handles sending and receiving data on the socket. When using
    /// `Lines`, we can work at the line level instead of having to manage the
    /// raw byte operations.
    bytes: Framed<TcpStream, BytesCodec>,

    /// Receive half of the message channel.
    ///
    /// This is used to receive messages from peers. When a message is received
    /// off of this `Rx`, it will be written to the socket.
    rx: Rx,

    stx: Tx,
}

impl Connection {
    /// Create a new instance of `Peer`.
    async fn new(
        state: Arc<Mutex<Shared>>,
        bytes: Framed<TcpStream, BytesCodec>,
        stx: Tx,
        
    ) -> io::Result<Connection> {
        // Get the client socket address
        let addr = bytes.get_ref().peer_addr()?;

        // Create a channel for this peer
        let (tx, rx) = mpsc::unbounded_channel();

        // Add an entry for this `Peer` in the shared state map.
        state.lock().await.peers.insert(addr, tx);

        Ok(Connection { bytes, rx, stx })
    }
}

async fn process(
    state: Arc<Mutex<Shared>>,
    stream: TcpStream,
    addr: SocketAddr,
    stx: Tx,
) -> Result<(), Box<dyn Error>> {
    let bytes = Framed::new(stream, BytesCodec::new());

    // Register our connection with state which internally sets up some channels.
    let mut connection = Connection::new(state.clone(), bytes, stx.clone()).await?;

    // Process incoming messages until our stream is exhausted by a disconnect.
    loop {
        tokio::select! {
            // A message was received from a peer. Send it to the current user.
            something = connection.rx.recv() =>  match something {
                    Some(msg) => connection.bytes.send(msg).await?,
                    None => tracing::info!("connection {:?} broken?", connection.bytes.get_ref().peer_addr())
            },

            result = connection.bytes.next() => match result {
                // A message was received from the current user, we should
                // broadcast this message to the other users.
                Some(Ok(msg)) => {
                    let mut state = state.lock().await;
                    state.broadcast(addr, &msg).await;
                    stx.send(msg).unwrap();
                }
                // An error occurred.
                Some(Err(e)) => {
                    tracing::info!(
                        "An error occurred while processing messages, error = {:?}",
                        e
                    );
                }
                // The stream has been exhausted.
                None => break,
            },
        }
    }

    // If this section is reached it means that the client was disconnected!
    // Let's let everyone still connected know about it.
    {
        tracing::info!("Connection to be removed");
        let mut state = state.lock().await;
        state.peers.remove(&addr);
        let msg = format!("{} has left the chat", &addr);
        tracing::info!("{}", msg);
    }

    Ok(())
}

async fn process_server_connection(
    state: Arc<Mutex<Shared>>,
    stream: StubbornTcpStream<SocketAddr>,
    addr: SocketAddr,
    mut srx: Rx,
) -> Result<(), Box<dyn Error>> {
    let mut bytes = Framed::new(stream, BytesCodec::new());

    // Process incoming messages until our stream is exhausted by a disconnect.
    loop {
        tokio::select! {
            // A message was received from a peer. Send it to the current user.
            
            result = bytes.next() => match result {
                // A message was received from the current user, we should
                // broadcast this message to the other users.
                Some(Ok(msg)) => {
                    let mut state = state.lock().await;
                    state.broadcast(addr, &msg).await;
                }
                // An error occurred.
                Some(Err(e)) => {
                    tracing::info!(
                        "An error occurred while processing messages, error = {:?}",
                        e
                    );
                }
                // The stream has been exhausted.
                None => break,
            },
            something = srx.recv() => { match something {
                Some(msg) => bytes.send(msg).await?,
                    None => tracing::info!("Something bad"),
            }
            }
        }
    }

    // If this section is reached it means that the client was disconnected!
    // Let's let everyone still connected know about it.
    {
        tracing::info!("Server broken");
    }

    Ok(())
}

