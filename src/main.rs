use futures::lock::Mutex as AsyncMutex;
use futures_channel::mpsc::unbounded;
use std::{
    collections::HashMap,
    env,
    io::{Error as IoError, Read, Write},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};
use tokio::net::TcpListener;
use websocket_shell::{async_stdio, handle_connection, read_buffer, PeerMap, CHILD_PROCESS_ID};
#[tokio::main]
async fn main() -> Result<(), IoError> {
    let addr = env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:8080".to_string());

    let mut command = Command::new("sh");

    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let mut out = command.spawn().unwrap();
    let mut command_in = out.stdin.take().unwrap();

    CHILD_PROCESS_ID.store(out.id(), std::sync::atomic::Ordering::Relaxed);

    command_in
        .write_all("echo 'Shell started'\n".as_bytes())
        .unwrap();
    let command_in = Arc::new(Mutex::new(command_in));
    let output_err = out.stderr.take().unwrap();
    let mut output = out.stdout.take().unwrap();
    let mut buf = [0; 32];
    output.read(&mut buf).unwrap();
    print!("{}", String::from_utf8(buf.to_vec()).unwrap());


    let (mut tx_out_put, rx_out_put) = unbounded();
    let (mut tx_err_out_put, rx_err_out_put) = unbounded();
    let rx_out_put = Arc::new(AsyncMutex::new(rx_out_put));
    let rx_err_out_put = Arc::new(AsyncMutex::new(rx_err_out_put));
    let state = PeerMap::new(AsyncMutex::new(HashMap::new()));

    tokio::spawn(async move {
        let decoder = async_stdio(output);
        read_buffer(decoder, &mut tx_out_put).await;
    });
    tokio::spawn(async move {
        let decoder = async_stdio(output_err);
        read_buffer(decoder, &mut tx_err_out_put).await;
    });

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
