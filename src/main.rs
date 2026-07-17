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
    layout::{Constraint, Direction, Layout, Rect, Alignment},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Sparkline, Wrap},
    Terminal,
};
use std::{
    fs::OpenOptions, 
    io::{self, Write}, 
    sync::{Arc, atomic::{AtomicBool, AtomicU8, Ordering}}, 
    time::Duration
};
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

// Вспомогательная функция для центрирования всплывающего окна
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ].as_ref())
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ].as_ref())
        .split(popup_layout[1])[1]
}

#[tokio::main]
async fn main() -> Result<(), io::Error> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (tx, mut rx) = mpsc::channel::<AppEvent>(100);
    
    let is_recording = Arc::new(AtomicBool::new(false));
    let rec_flag = is_recording.clone();
    
    let subject_age = Arc::new(AtomicU8::new(38));
    let age_flag = subject_age.clone();

    tokio::spawn(async move {
        run_ble_daemon(tx, rec_flag, age_flag).await;
    });

    let mut latest_data: Option<hrm::Telemetry> = None;
    let mut status_msg = String::from("SYS.INIT()...");
    let mut raw_log_stream: Vec<String> = vec![];
    
    let mut rmssd_history: Vec<u64> = vec![];
    let mut stress_history: Vec<u64> = vec![];

    // Флаг состояния всплывающего окна
    let mut show_manual = false;

    loop {
        let recording_active = is_recording.load(Ordering::Relaxed);
        let current_age = subject_age.load(Ordering::Relaxed);

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints([
                    Constraint::Length(3), // Header
                    Constraint::Min(20),   // Dashboard (Op / Base)
                    Constraint::Length(5), // IO Stream
                    Constraint::Length(3), // Footer
                ].as_ref())
                .split(f.area());

            let dashboard_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
                .split(chunks[1]);

            let left_panel = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(10), Constraint::Length(6)].as_ref())
                .split(dashboard_chunks[0]);

            let right_panel = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(10), Constraint::Length(6)].as_ref())
                .split(dashboard_chunks[1]);

            // HEADER
            let rec_text = if recording_active { "[ REC: ON ]" } else { "[ REC: OFF ]" };
            let rec_color = if recording_active { Color::Red } else { Color::DarkGray };
            let header = Paragraph::new(Line::from(vec![
                Span::styled(" GIBSON LINK v1.0.7  //  BIOMETRIC DUAL-CORE UPLINK ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                Span::styled(format!("  {} ", rec_text), Style::default().fg(rec_color).add_modifier(Modifier::RAPID_BLINK)),
                Span::styled(format!("  |  AGE: {} ", current_age), Style::default().fg(Color::Cyan)),
                Span::styled("  |  [M] MANUAL ", Style::default().fg(Color::Yellow)),
            ]))
            .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::Green)));
            f.render_widget(header, chunks[0]);

            if let Some(ref data) = latest_data {
                // ПАНЕЛЬ 1: ОПЕРАТИВНАЯ
                let op_lines = vec![
                    Line::from(vec![
                        Span::styled("HEART RATE:       ", Style::default().fg(Color::LightBlue)),
                        Span::styled(format!("{:03} BPM", data.bpm), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                    ]),
                    Line::from(vec![Span::styled(format!("HR ZONE:          {}", data.hr_zone), Style::default().fg(Color::Cyan))]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("RMSSD (FLEX):     ", Style::default().fg(Color::LightBlue)),
                        Span::styled(format!("{:05.1} ms -> {}", data.operative.rmssd, data.operative.rmssd_status), Style::default().fg(Color::Yellow)),
                    ]),
                    Line::from(vec![
                        Span::styled("pNN50 (PARASYMP): ", Style::default().fg(Color::LightBlue)),
                        Span::styled(format!("{:05.1} %  -> {}", data.operative.pnn50, data.operative.pnn50_status), Style::default().fg(Color::Yellow)),
                    ]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("POINCARE SD1:     ", Style::default().fg(Color::LightBlue)),
                        Span::styled(format!("{:05.1} ms -> {}", data.operative.sd1, data.operative.sd1_status), Style::default().fg(Color::Magenta)),
                    ]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("ANOMALIES (ECT):  ", Style::default().fg(Color::Red)),
                        Span::styled(format!("{} DETECTED", data.anomalies), Style::default().fg(Color::White)),
                    ]),
                ];
                let op_block = Paragraph::new(op_lines)
                    .block(Block::default().title(" OPERATIVE PROFILE (POLYGRAPH) ").borders(Borders::ALL).border_style(Style::default().fg(Color::LightBlue)));
                f.render_widget(op_block, left_panel[0]);

                // Ограничение графика в 50 элементов
                let rmssd_view = &rmssd_history[rmssd_history.len().saturating_sub(50)..];
                let rmssd_spark = Sparkline::default()
                    .block(Block::default().title(" RMSSD TREND ").borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)))
                    .data(rmssd_view)
                    .style(Style::default().fg(Color::Yellow));
                f.render_widget(rmssd_spark, left_panel[1]);

                // ПАНЕЛЬ 2: БАЗОВАЯ
                let base_lines = vec![
                    Line::from(vec![Span::styled(format!("RESPIRATORY EST:  {:04.1} BR/MIN", data.resp_rate), Style::default().fg(Color::Green))]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("STRESS INDEX:     ", Style::default().fg(Color::Green)),
                        Span::styled(format!("{:06.1} -> {}", data.baseline.baevsky_index, data.baseline.stress_status), Style::default().fg(Color::LightRed)),
                    ]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("SDNN (RESIDUAL):  ", Style::default().fg(Color::Green)),
                        Span::styled(format!("{:05.1} ms -> {}", data.baseline.sdnn, data.baseline.sdnn_status), Style::default().fg(Color::Yellow)),
                    ]),
                    Line::from(vec![
                        Span::styled("CV (STABILITY):   ", Style::default().fg(Color::Green)),
                        Span::styled(format!("{:05.1} %  -> {}", data.baseline.cv, data.baseline.cv_status), Style::default().fg(Color::Yellow)),
                    ]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("POINCARE SD2:     ", Style::default().fg(Color::Green)),
                        Span::styled(format!("{:05.1} ms -> {}", data.baseline.sd2, data.baseline.sd2_status), Style::default().fg(Color::Magenta)),
                    ]),
                ];
                let base_block = Paragraph::new(base_lines)
                    .block(Block::default().title(" BASELINE PROFILE (ECG) ").borders(Borders::ALL).border_style(Style::default().fg(Color::Green)));
                f.render_widget(base_block, right_panel[0]);

                // Ограничение графика в 50 элементов
                let stress_view = &stress_history[stress_history.len().saturating_sub(50)..];
                let stress_spark = Sparkline::default()
                    .block(Block::default().title(" STRESS INDEX TREND ").borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)))
                    .data(stress_view)
                    .style(Style::default().fg(Color::LightRed));
                f.render_widget(stress_spark, right_panel[1]);

            } else {
                let wait_block = Paragraph::new("AWAITING DUAL-CORE TELEMETRY...")
                    .alignment(Alignment::Center)
                    .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::Green)));
                f.render_widget(wait_block, chunks[1]);
            }

            // RAW DATA STREAM
            let raw_text: Vec<Line> = raw_log_stream.iter().map(|s| Line::from(Span::styled(s, Style::default().fg(Color::DarkGray)))).collect();
            let raw_block = Paragraph::new(raw_text)
                .block(Block::default().title(" I/O STREAM ").borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)));
            f.render_widget(raw_block, chunks[2]);

            // FOOTER
            let footer = Paragraph::new(format!("{} | HOTKEYS: [Q]uit | [R]ecord | [+/-] Adjust Age", status_msg))
                .style(Style::default().fg(Color::Green))
                .block(Block::default().title(" DAEMON STATUS ").borders(Borders::ALL).border_style(Style::default().fg(Color::Green)));
            f.render_widget(footer, chunks[3]);

            // МАНУАЛ (Отрисовка поверх всего)
            if show_manual {
                let popup_area = centered_rect(75, 75, f.area());
                
                // Очищаем фон под окном
                f.render_widget(Clear, popup_area);

                let manual_text = vec![
                    Line::from(Span::styled("УПРАВЛЕНИЕ:", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
                    Line::from("  [Q] / [Esc] — Закрыть терминал"),
                    Line::from("  [R] — Запись данных в лог-файл (Вкл/Выкл)"),
                    Line::from("  [+] / [-] — Калибровка возраста (динамический пересчет зон)"),
                    Line::from("  [M] — Показать/Скрыть этот мануал"),
                    Line::from(""),
                    Line::from(Span::styled("БАЗОВЫЕ МЕТРИКИ:", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
                    Line::from(vec![Span::styled("  RMSSD (Гибкость): ", Style::default().fg(Color::Yellow)), Span::raw("Твоя способность расслабляться в реальном времени. Чувствителен к каждой мысли. Упал — ты напрягся. Выше 40 — ты спокоен.")]),
                    Line::from(vec![Span::styled("  Индекс Баевского (Стресс): ", Style::default().fg(Color::LightRed)), Span::raw("Твоя плата за выживание. Показывает, сколько ресурсов тело сжигает прямо сейчас. Выше 150 — ты работаешь на износ.")]),
                    Line::from(vec![Span::styled("  SDNN (Батарейка): ", Style::default().fg(Color::Yellow)), Span::raw("Общий запас прочности на длинной дистанции. Если показатель рухнул ниже 30 — система истощена, требуется сон.")]),
                    Line::from(vec![Span::styled("  pNN50 (Парасимпатика): ", Style::default().fg(Color::Yellow)), Span::raw("Твоя \"педаль тормоза\". Показывает, способна ли нервная система сама себя успокоить. Меньше 3% — тормоза отказали.")]),
                    Line::from(vec![Span::styled("  CV (Стабильность): ", Style::default().fg(Color::Yellow)), Span::raw("Насколько ровно работает мотор. Меньше 2% — пульс зажат стрессом как метроном (это плохо). Норма 2-6%.")]),
                    Line::from(vec![Span::styled("  Аномалии (ECT): ", Style::default().fg(Color::Red)), Span::raw("Детектор сбоев ритма. Ловит преждевременные или пропущенные удары (экстрасистолы).")]),
                ];

                let popup_block = Paragraph::new(manual_text)
                    .block(Block::default().title(" USER MANUAL ").borders(Borders::ALL).border_style(Style::default().fg(Color::Cyan)))
                    .wrap(Wrap { trim: true });

                f.render_widget(popup_block, popup_area);
            }
        })?;

        if let Ok(event) = rx.try_recv() {
            match event {
                AppEvent::HrData(data) => {
                    let log_str = format!("> SYS.RECV: HR:{} RR:{:?}", data.bpm, data.rr_ms);
                    raw_log_stream.push(log_str);
                    if raw_log_stream.len() > 3 { raw_log_stream.remove(0); }
                    
                    rmssd_history.push(data.operative.rmssd as u64);
                    if rmssd_history.len() > 150 { rmssd_history.remove(0); } 
                    
                    stress_history.push(data.baseline.baevsky_index as u64);
                    if stress_history.len() > 150 { stress_history.remove(0); }

                    latest_data = Some(data);
                }
                AppEvent::Status(msg) => { status_msg = msg; }
            }
        }

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('m') | KeyCode::Char('M') | KeyCode::Char('ь') | KeyCode::Char('Ь') => {
                        show_manual = !show_manual;
                    }
                    KeyCode::Char('r') | KeyCode::Char('R') | KeyCode::Char('к') | KeyCode::Char('К') => {
                        is_recording.fetch_xor(true, Ordering::Relaxed);
                    }
                    KeyCode::Char('+') | KeyCode::Up => {
                        let current = subject_age.load(Ordering::Relaxed);
                        if current < 120 { subject_age.store(current + 1, Ordering::Relaxed); }
                    }
                    KeyCode::Char('-') | KeyCode::Down => {
                        let current = subject_age.load(Ordering::Relaxed);
                        if current > 10 { subject_age.store(current - 1, Ordering::Relaxed); }
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

async fn run_ble_daemon(tx: mpsc::Sender<AppEvent>, rec_flag: Arc<AtomicBool>, age_flag: Arc<AtomicU8>) {
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
                        let current_age = age_flag.load(Ordering::Relaxed);
                        if let Some(telemetry) = analyzer.process_payload(&data.value, current_age) {
                            
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
                                        telemetry.baseline.rmssd, telemetry.baseline.sdnn, telemetry.baseline.baevsky_index);
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