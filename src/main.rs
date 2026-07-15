mod hrm;

use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::Manager;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Alignment},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Terminal,
};
use std::{fs::OpenOptions, io::{self, Write}, sync::{Arc, atomic::{AtomicBool, Ordering}}, time::Duration};
use tokio::{sync::mpsc, time::timeout};
use futures::stream::StreamExt;

const HR_CHARACTERISTIC: &str = "00002a37-0000-1000-8000-00805f9b34fb";
const TARGET_MAC: &str = "D7:3B:5E:CF:DA:3F";
const LOG_FILE: &str = "sys_telemetry.log";
const MAX_LOG_SIZE: u64 = 5 * 1024 * 1024;

enum AppEvent {
    HrData(hrm::Telemetry),
    Status(String),
}

#[tokio::main]
async fn main() -> Result<(), io::Error> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (tx, mut rx) = mpsc::channel::<AppEvent>(100);
    let is_recording = Arc::new(AtomicBool::new(false)); // По умолчанию запись выключена
    let rec_flag = is_recording.clone();

    tokio::spawn(async move {
        run_ble_daemon(tx, rec_flag).await;
    });

    let mut latest_data: Option<hrm::Telemetry> = None;
    let mut status_msg = String::from("SYS.INIT()...");
    let mut raw_log_stream: Vec<String> = vec![];

    loop {
        let recording_active = is_recording.load(Ordering::Relaxed);

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(10),
                    Constraint::Length(3),
                ].as_ref())
                .split(f.size());

            let main_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(40), Constraint::Percentage(60)].as_ref())
                .split(chunks[1]);

            // HEADER
            let rec_text = if recording_active { "[ REC: ON ]" } else { "[ REC: OFF ]" };
            let rec_color = if recording_active { Color::Red } else { Color::DarkGray };
            let header = Paragraph::new(Line::from(vec![
                Span::styled(" GIBSON LINK v1.0.4  //  BIOMETRIC UPLINK ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                Span::styled(format!("  {} ", rec_text), Style::default().fg(rec_color).add_modifier(Modifier::RAPID_BLINK)),
            ]))
            .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::Green)));
            f.render_widget(header, chunks[0]);

            // RAW DATA STREAM (LEFT)
            let raw_text: Vec<Line> = raw_log_stream.iter().map(|s| Line::from(Span::styled(s, Style::default().fg(Color::DarkGray)))).collect();
            let raw_block = Paragraph::new(raw_text)
                .block(Block::default().title(" I/O STREAM ").borders(Borders::ALL).border_style(Style::default().fg(Color::Green)));
            f.render_widget(raw_block, main_chunks[0]);

            // ANALYTICS (RIGHT)
            if let Some(ref data) = latest_data {
                let analytics = vec![
                    Line::from(vec![Span::styled(format!("SUBJECT AGE:      {} YRS", data.age), Style::default().fg(Color::Green))]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("HEART RATE:       ", Style::default().fg(Color::Green)),
                        Span::styled(format!("{:03} BPM", data.bpm), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                    ]),
                    Line::from(vec![Span::styled(format!("HR ZONE:          {}", data.hr_zone), Style::default().fg(Color::Cyan))]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("RMSSD (FLEX):     ", Style::default().fg(Color::Green)),
                        Span::styled(format!("{:05.1} ms  -> {}", data.rmssd, data.rmssd_status), Style::default().fg(Color::Yellow)),
                    ]),
                    Line::from(vec![
                        Span::styled("SDNN (RESIDUAL):  ", Style::default().fg(Color::Green)),
                        Span::styled(format!("{:05.1} ms", data.sdnn), Style::default().fg(Color::Yellow)),
                    ]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("STRESS INDEX:     ", Style::default().fg(Color::Green)),
                        Span::styled(format!("{:06.1}     -> {}", data.baevsky_index, data.stress_status), Style::default().fg(Color::LightRed)),
                    ]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("RESPIRATORY EST:  ", Style::default().fg(Color::Green)),
                        Span::styled(format!("{:04.1} BR/MIN", data.resp_rate), Style::default().fg(Color::Cyan)),
                    ]),
                ];
                let analytics_block = Paragraph::new(analytics)
                    .block(Block::default().title(" VITAL SIGNS ANALYSIS ").borders(Borders::ALL).border_style(Style::default().fg(Color::Green)));
                f.render_widget(analytics_block, main_chunks[1]);
            } else {
                let wait_block = Paragraph::new("AWAITING TELEMETRY PACKETS...")
                    .alignment(Alignment::Center)
                    .block(Block::default().title(" VITAL SIGNS ANALYSIS ").borders(Borders::ALL).border_style(Style::default().fg(Color::Green)));
                f.render_widget(wait_block, main_chunks[1]);
            }

            // FOOTER / STATUS
            let footer = Paragraph::new(status_msg.clone())
                .style(Style::default().fg(Color::Green))
                .block(Block::default().title(" DAEMON STATUS ").borders(Borders::ALL).border_style(Style::default().fg(Color::Green)));
            f.render_widget(footer, chunks[2]);
        })?;

        if let Ok(event) = rx.try_recv() {
            match event {
                AppEvent::HrData(data) => {
                    let log_str = format!("> SYS.RECV: HR:{} RR:{:?}", data.bpm, data.rr_ms);
                    raw_log_stream.push(log_str);
                    if raw_log_stream.len() > 15 { raw_log_stream.remove(0); }
                    latest_data = Some(data);
                }
                AppEvent::Status(msg) => { status_msg = msg; }
            }
        }

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('r') | KeyCode::Char('R') | KeyCode::Char('к') | KeyCode::Char('К') => {
                        is_recording.fetch_xor(true, Ordering::Relaxed);
                    }
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
}

async fn run_ble_daemon(tx: mpsc::Sender<AppEvent>, rec_flag: Arc<AtomicBool>) {
    let mut analyzer = hrm::BioAnalyzer::default();
    let _ = tx.send(AppEvent::Status("> INITIALIZING KERNEL DRIVERS...".into())).await;
    
    let manager = match Manager::new().await {
        Ok(m) => m, Err(_) => return,
    };
    
    let adapters = manager.adapters().await.unwrap_or_default();
    if adapters.is_empty() {
        let _ = tx.send(AppEvent::Status("> ERR: NO BLUETOOTH ADAPTER DETECTED".into())).await;
        return;
    }
    let central = &adapters[0];

    loop {
        let _ = tx.send(AppEvent::Status("> SCANNING LOCAL FREQUENCIES...".into())).await;
        let _ = central.start_scan(ScanFilter::default()).await;

        let mut target_device = None;
        while target_device.is_none() {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if let Ok(peripherals) = central.peripherals().await {
                for p in peripherals {
                    if let Ok(Some(props)) = p.properties().await {
                        if props.address.to_string() == TARGET_MAC {
                            target_device = Some(p);
                            break;
                        }
                    }
                }
            }
        }

        let device = target_device.unwrap();
        let _ = tx.send(AppEvent::Status("> MAC LOCKED. BYPASSING HANDSHAKE...".into())).await;

        if device.connect().await.is_err() { continue; }
        if device.discover_services().await.is_err() { 
            let _ = device.disconnect().await; continue; 
        }

        let chars = device.characteristics();
        let hr_char = match chars.iter().find(|c| c.uuid.to_string() == HR_CHARACTERISTIC) {
            Some(c) => c,
            None => { let _ = device.disconnect().await; continue; }
        };

        if device.subscribe(hr_char).await.is_err() {
            let _ = device.disconnect().await; continue;
        }

        let _ = tx.send(AppEvent::Status("> CONNECTION SECURED. INTERCEPTING DATA STREAM...".into())).await;

        let mut notification_stream = match device.notifications().await {
            Ok(s) => s, Err(_) => { let _ = device.disconnect().await; continue; }
        };

        loop {
            match timeout(Duration::from_secs(5), notification_stream.next()).await {
                Ok(Some(data)) => {
                    if data.uuid.to_string() == HR_CHARACTERISTIC {
                        if let Some(telemetry) = analyzer.process_payload(&data.value) {
                            
                            let _ = tx.send(AppEvent::HrData(telemetry.clone())).await;

                            if rec_flag.load(Ordering::Relaxed) {
                                let file_size = std::fs::metadata(LOG_FILE).map(|m| m.len()).unwrap_or(0);
                                let mut opts = OpenOptions::new();
                                opts.create(true).write(true);
                                if file_size >= MAX_LOG_SIZE { opts.truncate(true); } else { opts.append(true); }
                                
                                if let Ok(mut file) = opts.open(LOG_FILE) {
                                    let timestamp = chrono::Utc::now().to_rfc3339();
                                    let csv = format!("{},{},{},{:.1},{:.1},{:.1}\n", 
                                        timestamp, telemetry.bpm, telemetry.hr_zone, 
                                        telemetry.rmssd, telemetry.sdnn, telemetry.baevsky_index);
                                    let _ = file.write_all(csv.as_bytes());
                                }
                            }
                        }
                    }
                }
                Ok(None) | Err(_) => break,
            }
        }
        
        let _ = tx.send(AppEvent::Status("> LINK SEVERED. ATTEMPTING RECONNECT...".into())).await;
        let _ = device.disconnect().await;
    }
}