use futures::lock::Mutex as AsyncMutex;
use futures_channel::mpsc::unbounded;
use std::{
    collections::HashMap,
    env,
    io::Error as IoError,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};
use tokio::net::TcpListener;
use websocket_shell::{handle_connection, read_buffer, PeerMap, CHILD_PROCESS_ID};
#[tokio::main]
async fn main() -> Result<(), IoError> {
    let addr = env::args().nth(1).unwrap_or("127.0.0.1:8080".into());

    let mut command = Command::new("sh");

    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let mut out = command.spawn().unwrap();
    let command_in = out.stdin.take().unwrap();

    CHILD_PROCESS_ID.store(out.id(), std::sync::atomic::Ordering::Relaxed);

    let command_in = Arc::new(Mutex::new(command_in));
    let output_err = out.stderr.take().unwrap();
    let output = out.stdout.take().unwrap();

    let (tx_out_put, rx_out_put) = unbounded();
    let (tx_err_out_put, rx_err_out_put) = unbounded();
    let rx_out_put = Arc::new(AsyncMutex::new(rx_out_put));
    let rx_err_out_put = Arc::new(AsyncMutex::new(rx_err_out_put));
    let state = PeerMap::new(AsyncMutex::new(HashMap::new()));

    tokio::spawn(read_buffer(output, tx_out_put));
    tokio::spawn(read_buffer(output_err, tx_err_out_put));

    let listener = TcpListener::bind(&addr).await?;
    println!("Listening on: {}", addr);

    while let Ok((stream, addr)) = listener.accept().await {
        tokio::spawn(handle_connection(
            state.clone(),
            command_in.clone(),
            rx_out_put.clone(),
            rx_err_out_put.clone(),
            stream,
            addr,
        ));
    }
    Ok(())
}
