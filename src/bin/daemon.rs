#[path = "../hrm.rs"]
mod hrm;

use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::Manager;
use futures::stream::StreamExt;
use rusqlite::{params, Connection};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;

const HR_CHARACTERISTIC: &str = "00002a37-0000-1000-8000-00805f9b34fb";
const TARGET_MAC: &str = "D7:3B:5E:CF:DA:3F";

struct DaemonState {
    pub is_recording: bool,
    pub bt_connected: bool,
    pub status_msg: String,
}

fn setup_db() -> Connection {
    let conn = Connection::open("telemetry.db").expect("Failed to open db");
    conn.execute(
        "CREATE TABLE IF NOT EXISTS metrics_agg (
            timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
            bpm INTEGER, rmssd REAL, baevsky REAL
        )", [],
    ).unwrap();
    conn
}

#[tokio::main]
async fn main() {
    println!("SYS: BOOTING GIBSON DAEMON...");
    
    let db_conn = Arc::new(Mutex::new(setup_db()));
    let analyzer = Arc::new(Mutex::new(hrm::BioAnalyzer::default()));
    
    let state = Arc::new(Mutex::new(DaemonState {
        is_recording: true,
        bt_connected: false,
        status_msg: "BOOTING".into(),
    }));

    let (tx, _) = broadcast::channel::<hrm::DaemonPayload>(16);
    
    let data_tx = tx.clone();
    tokio::spawn(async move {
        let listener = TcpListener::bind("127.0.0.1:8080").await.unwrap();
        loop {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut rx = data_tx.subscribe();
                tokio::spawn(async move {
                    while let Ok(msg) = rx.recv().await {
                        if let Ok(json) = serde_json::to_string(&msg) {
                            if socket.write_all(format!("{}\n", json).as_bytes()).await.is_err() { break; }
                        }
                    }
                });
            }
        }
    });

    let ctrl_state = state.clone();
    let ctrl_analyzer = analyzer.clone();
    let ctrl_db = db_conn.clone();
    tokio::spawn(async move {
        let listener = TcpListener::bind("127.0.0.1:8081").await.unwrap();
        println!("SYS: CONTROL PLANE LISTENING ON 8081");
        loop {
            if let Ok((mut socket, _)) = listener.accept().await {
                let ctrl_s = ctrl_state.clone();
                let ctrl_a = ctrl_analyzer.clone();
                let ctrl_d = ctrl_db.clone();
                tokio::spawn(async move {
                    let mut buf = [0; 1024];
                    while let Ok(n) = socket.read(&mut buf).await {
                        if n == 0 { break; }
                        let cmd = String::from_utf8_lossy(&buf[..n]).trim().to_uppercase();
                        match cmd.as_str() {
                            "PAUSE" => { ctrl_s.lock().await.is_recording = false; let _ = socket.write_all(b"ACK: RECORDING PAUSED\n").await; }
                            "RESUME" => { ctrl_s.lock().await.is_recording = true; let _ = socket.write_all(b"ACK: RECORDING RESUMED\n").await; }
                            "RESET" => { ctrl_a.lock().await.clear(); let _ = socket.write_all(b"ACK: ANALYZER BUFFER CLEARED\n").await; }
                            "PURGE" => { let _ = ctrl_d.lock().await.execute("DELETE FROM metrics_agg", []); let _ = socket.write_all(b"ACK: SQLITE DB PURGED\n").await; }
                            "LOGS" => {
                                let st = ctrl_s.lock().await;
                                let log = format!("STATUS: {}\nBT_CONNECTED: {}\nRECORDING: {}\n", st.status_msg, st.bt_connected, st.is_recording);
                                let _ = socket.write_all(log.as_bytes()).await;
                            }
                            "SHUTDOWN" => { let _ = socket.write_all(b"ACK: INITIATING SHUTDOWN...\n").await; std::process::exit(0); }
                            "HELP" => { let _ = socket.write_all(b"CMDS: PAUSE | RESUME | RESET | PURGE | LOGS | SHUTDOWN\n").await; }
                            _ => { let _ = socket.write_all(b"ERR: UNKNOWN COMMAND\n").await; }
                        }
                    }
                });
            }
        }
    });

    let manager = Manager::new().await.unwrap();
    let adapters = manager.adapters().await.unwrap();
    let central = &adapters[0];

    loop {
        { state.lock().await.status_msg = "SCANNING".into(); state.lock().await.bt_connected = false; }
        let _ = tx.send(hrm::DaemonPayload { daemon_state: get_state(&state).await, sys_msg: "SCANNING...".into(), telemetry: None });
        
        let _ = central.start_scan(ScanFilter::default()).await;
        
        let mut target_device = None;
        while target_device.is_none() {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if let Ok(peripherals) = central.peripherals().await {
                for p in peripherals {
                    if let Ok(Some(props)) = p.properties().await {
                        if props.address.to_string() == TARGET_MAC { target_device = Some(p); break; }
                    }
                }
            }
        }

        let device = target_device.unwrap();
        if device.connect().await.is_err() { continue; }
        if device.discover_services().await.is_err() { continue; }

        let chars = device.characteristics();
        let hr_char = match chars.iter().find(|c| c.uuid.to_string() == HR_CHARACTERISTIC) {
            Some(c) => c, None => continue,
        };
        if device.subscribe(hr_char).await.is_err() { continue; }
        
        { state.lock().await.status_msg = "LINK ACTIVE".into(); state.lock().await.bt_connected = true; }
        let mut stream = device.notifications().await.unwrap();

        loop {
            match timeout(Duration::from_secs(5), stream.next()).await {
                Ok(Some(data)) => {
                    if data.uuid.to_string() == HR_CHARACTERISTIC {
                        let mut a = analyzer.lock().await;
                        if let Some(telemetry) = a.process_payload(&data.value, 38) {
                            let current_state = get_state(&state).await;
                            let is_rec = state.lock().await.is_recording;

                            let _ = tx.send(hrm::DaemonPayload {
                                daemon_state: current_state,
                                sys_msg: "STREAMING".into(),
                                telemetry: Some(telemetry.clone()),
                            });

                            if is_rec {
                                let db = Arc::clone(&db_conn);
                                tokio::spawn(async move {
                                    let conn = db.lock().await;
                                    let _ = conn.execute(
                                        "INSERT INTO metrics_agg (bpm, rmssd, baevsky) VALUES (?1, ?2, ?3)",
                                        params![telemetry.bpm, telemetry.baseline.rmssd, telemetry.baseline.baevsky_index],
                                    );
                                    let _ = conn.execute("DELETE FROM metrics_agg WHERE timestamp <= datetime('now', '-1 day')", []);
                                });
                            }
                        }
                    }
                }
                Ok(None) | Err(_) => break,
            }
        }
        analyzer.lock().await.clear();
        let _ = device.disconnect().await;
    }
}

async fn get_state(state: &Arc<Mutex<DaemonState>>) -> String {
    let s = state.lock().await;
    if !s.is_recording { "PAUSED".into() } else if s.bt_connected { "RUNNING".into() } else { "SEARCHING".into() }
}