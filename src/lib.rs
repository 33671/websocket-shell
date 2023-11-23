use async_stream::stream;
use futures::{lock::Mutex as AsyncMutex, SinkExt};
use futures_channel::mpsc::{unbounded, UnboundedSender};
use futures_util::{future, pin_mut, Stream, StreamExt};
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::SocketAddr,
    process::{ChildStdin, Command},
    sync::{
        atomic::{AtomicI32, AtomicU32, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{net::TcpStream, task::spawn_blocking, time};
use tokio_tungstenite::tungstenite::Message;
use utf8_decode::Decoder;
type Tx = UnboundedSender<u32>;
type OutputReceiver = Arc<AsyncMutex<futures_channel::mpsc::UnboundedReceiver<String>>>;
pub type PeerMap = Arc<AsyncMutex<HashMap<SocketAddr, Tx>>>;
pub const BUFFER_SIZE: usize = 128;
pub static BUFFER_COUNTER: AtomicI32 = AtomicI32::new(0);
pub static CHILD_PROCESS_ID: AtomicU32 = AtomicU32::new(0);
pub fn async_stdio<R>(reader: R) -> impl futures::Stream<Item = char>
where
    R: Read + Send + 'static,
{
    let decoder = Decoder::new(reader.bytes().map(|x| x.unwrap_or(' ' as u8)));
    let decoder = Arc::new(Mutex::new(decoder));
    stream! {
        loop {
            let decoder = decoder.clone();
            match spawn_blocking(move || decoder.lock().unwrap().next()).await.unwrap_or(Some(Ok('?'))) {
                None => continue,
                Some(result) => {
                    match result {
                        Ok(res) =>{print!("{res}"); yield res},
                        Err(_) => {
                            println!("wrong");
                            yield '?';
                            continue;
                        }
                    }
                }
            }
        }
    }
}
pub async fn handle_connection(
    peer_map: PeerMap,
    command_input: Arc<Mutex<ChildStdin>>,
    receiver: OutputReceiver,
    error_receiver: OutputReceiver,
    raw_stream: TcpStream,
    addr: SocketAddr,
) {
    println!("Incoming connection from: {}", addr);

    let ws_stream = tokio_tungstenite::accept_async(raw_stream)
        .await
        .expect("Error during the websocket handshake occurred");
    println!("Connection established: {}", addr);

    let (tx, mut rx) = unbounded();
    {
        let mut peers = peer_map.lock().await;
        peers.iter().map(|(_, ws_sink)| ws_sink).for_each(|x| {
            let _ = x.unbounded_send(0);
        });
        peers.clear();
        peers.insert(addr, tx);
    }
    let mut receiver_lock = receiver.lock().await;
    let mut next_err = error_receiver.lock().await;

    let (mut outgoing, mut incoming) = ws_stream.split();
    let run_command = async move {
        loop {
            let msg_res = incoming.next().await;
            if msg_res.is_none() {
                continue;
            }
            let msg_res = msg_res.unwrap();
            if msg_res.is_err() {
                return;
            }
            let msg = msg_res.unwrap();
            if !msg.is_text() {
                return;
            }
            let mut command_txt = msg.to_string();
            if !command_txt.ends_with("\n") {
                command_txt.push('\n');
            }
            if command_txt.trim().starts_with("ctrl+c") {
                kill_children(CHILD_PROCESS_ID.load(Ordering::Acquire));
            } else {
                command_input
                    .lock()
                    .unwrap()
                    .write_all(command_txt.as_bytes())
                    .unwrap();
            }
        }
    };

    let send_output = async move {
        loop {
            let string_out = receiver_lock.next();
            let string_out_err = next_err.next();
            let receive_string_result = future::select(string_out, string_out_err).await;
            BUFFER_COUNTER.fetch_sub(1, Ordering::Relaxed);
            let result_string = match receive_string_result {
                future::Either::Left((value1, _)) => value1,
                future::Either::Right((value1, _)) => value1,
            };
            if let Some(output_buff) = result_string {
                if outgoing.send(Message::Text(output_buff)).await.is_err() {
                    return;
                }
            }
        }
    };
    pin_mut!(run_command, send_output);
    let selecta = future::select(run_command, send_output);
    future::select(selecta, rx.next()).await;
    println!("{} disconnected", &addr);
}

pub async fn read_buffer(
    rx_char: impl Stream<Item = char>,
    tx_out_put: &mut futures_channel::mpsc::UnboundedSender<String>,
) {
    let mut send_buffer: Vec<char> = Vec::with_capacity(BUFFER_SIZE);
    pin_mut!(rx_char);
    loop {
        let mut send = false;
        let received = match time::timeout(Duration::from_millis(200), rx_char.next()).await {
            Ok(next) => next,
            Err(_) => {
                send = true;
                None
            }
        };
        if received.is_some() {
            let char_res = received.unwrap();
            send_buffer.push(char_res);
            if send_buffer.len() >= BUFFER_SIZE {
                send = true;
            }
        }
        if send {
            if send_buffer.len() == 0 {
                continue;
            }
            let result = String::from_iter(&send_buffer);
            send_buffer.clear();
            if result.is_empty() || BUFFER_COUNTER.load(Ordering::Acquire) > 200 {
                continue;
            }
            if tx_out_put.send(result).await.is_ok() {
                BUFFER_COUNTER.fetch_add(1, Ordering::Relaxed);
                // println!("{:?}", BUFFER_COUNTER);
            }
        }
    }
}

fn kill_children(process_id: u32) {
    let find_child_command = Command::new(format!("pgrep"))
        .arg("-P")
        .arg(process_id.to_string())
        .output()
        .unwrap();
    String::from_utf8(find_child_command.stdout)
        .unwrap()
        .lines()
        .for_each(|x| {
            Command::new(format!("kill"))
                .arg("-INT")
                .arg(x.trim())
                .output()
                .unwrap();
        });
}
