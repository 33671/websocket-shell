use futures::{executor::block_on, lock::Mutex as AsyncMutex, SinkExt};
use futures_channel::mpsc::unbounded;
use futures_util::{future, pin_mut, StreamExt};
use std::{
    env,
    io::{Error as IoError, Read, Write},
    net::SocketAddr,
    process::{ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicI32, AtomicU32, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use utf8_read::{Char, Reader};
type OutputReceiver = Arc<AsyncMutex<futures_channel::mpsc::UnboundedReceiver<String>>>;
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
async fn handle_connection(
    command_input: Arc<Mutex<ChildStdin>>,
    receiver: OutputReceiver,
    error_receiver: OutputReceiver,
    raw_stream: TcpStream,
    addr: SocketAddr,
) {
    println!("Incoming TCP connection from: {}", addr);

    let ws_stream = tokio_tungstenite::accept_async(raw_stream)
        .await
        .expect("Error during the websocket handshake occurred");
    println!("Connection established: {}", addr);
    let (mut outgoing, mut incoming) = ws_stream.split();
    let receiver_lock = receiver.try_lock();
    if receiver_lock.is_none() {
        let _ = outgoing
            .send(Message::Text("Another client has already connected".into()))
            .await;
        return;
    }
    let mut next_err = error_receiver.lock().await;
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
            println!(
                "Received a command from {}: {}",
                addr,
                msg.to_text().unwrap()
            );
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

    let mut next = receiver_lock.unwrap();
    let send_output = async move {
        loop {
            let string_out = next.next();
            let string_out_err = next_err.next();
            let receive_string_result = future::select(string_out, string_out_err).await;
            BUFFER_COUNTER.fetch_sub(1, Ordering::Relaxed);
            let result_string: Option<String>;
            match receive_string_result {
                future::Either::Left((value1, _)) => {
                    result_string = value1;
                }
                future::Either::Right((value1, _)) => {
                    result_string = value1;
                }
            }
            if let Some(output_buff) = result_string {
                if outgoing.send(Message::Text(output_buff)).await.is_err() {
                    return;
                }
            }
        }
    };
    pin_mut!(run_command, send_output);
    future::select(run_command, send_output).await;
    println!("{} disconnected", &addr);

    // kill_children(CHILD_PROCESS_ID.load(Ordering::Acquire));
}
const BUFFER_SIZE: usize = 128;
static BUFFER_COUNTER: AtomicI32 = AtomicI32::new(0);
static CHILD_PROCESS_ID: AtomicU32 = AtomicU32::new(0);
fn read_buffer(
    rx_char: std::sync::mpsc::Receiver<Result<Char, utf8_read::Error>>,
    tx_out_put: &mut futures_channel::mpsc::UnboundedSender<String>,
) {
    let mut send_buffer: Vec<char> = Vec::with_capacity(BUFFER_SIZE);
    loop {
        let mut send = false;
        let received = rx_char.recv_timeout(Duration::from_millis(100));
        if received.is_ok() {
            let char_res = received.unwrap().unwrap_or(Char::Char('?'));
            match char_res {
                Char::NoData => {
                    panic!("program exit");
                }
                Char::Eof => {
                    panic!("read to End of Line");
                }
                Char::Char(origin_char) => {
                    if send_buffer.len() >= BUFFER_SIZE {
                        send = true;
                    }
                    send_buffer.push(origin_char);
                }
            }
        } else {
            send = true;
        }
        if send {
            let result = String::from_iter(&send_buffer);
            send_buffer.clear();
            if !result.is_empty() {
                if BUFFER_COUNTER.load(Ordering::Acquire) > 200 {
                    continue;
                }
                block_on(tx_out_put.send(result)).unwrap();
                BUFFER_COUNTER.fetch_add(1, Ordering::Relaxed);
                // println!("{:?}", BUFFER_COUNTER);
            }
        }
    }
}
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

    let (tx_char, rx_char) = std::sync::mpsc::channel();
    let (tx_err_char, rx_err_char) = std::sync::mpsc::channel();
    let (mut tx_out_put, rx_out_put) = unbounded();
    let (mut tx_err_out_put, rx_err_out_put) = unbounded();
    let rx_out_put = Arc::new(AsyncMutex::new(rx_out_put));
    let rx_err_out_put = Arc::new(AsyncMutex::new(rx_err_out_put));

    thread::spawn(move || {
        let mut utf8reader = Reader::new(output);
        loop {
            //this will block current thread
            let next = utf8reader.next_char();
            tx_char.send(next).unwrap();
        }
    });
    thread::spawn(move || {
        let mut utf8reader = Reader::new(output_err);
        loop {
            //this will block current thread
            let next = utf8reader.next_char();
            tx_err_char.send(next).unwrap();
        }
    });
    thread::spawn(move || {
        read_buffer(rx_char, &mut tx_out_put);
    });
    thread::spawn(move || {
        read_buffer(rx_err_char, &mut tx_err_out_put);
    });

    let listener = TcpListener::bind(&addr).await?;
    println!("Listening on: {}", addr);

    while let Ok((stream, addr)) = listener.accept().await {
        tokio::spawn(handle_connection(
            command_in.clone(),
            rx_out_put.clone(),
            rx_err_out_put.clone(),
            stream,
            addr,
        ));
    }
    Ok(())
}
