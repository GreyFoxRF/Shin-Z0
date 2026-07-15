mod hrm;

use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::Manager;
use std::time::Duration;
use futures::stream::StreamExt;
use std::fs::OpenOptions;
use std::io::Write;
use tracing::{info, error};
use tokio::time::timeout;

const HR_CHARACTERISTIC: &str = "00002a37-0000-1000-8000-00805f9b34fb";
const TARGET_MAC: &str = "D7:3B:5E:CF:DA:3F";
const LOG_FILE: &str = "hr_data_log.csv";
const MAX_LOG_SIZE: u64 = 5 * 1024 * 1024; // 5 МБ

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    info!("Shin-Z0: System initialized.");

    let manager = Manager::new().await.unwrap();
    let adapters = manager.adapters().await.unwrap();

    if adapters.is_empty() {
        error!("Критическая ошибка: Bluetooth адаптер не найден.");
        return;
    }

    let central = &adapters[0];
    info!("Hardware: {:?}", central);

    // Внешний цикл демона (обеспечивает авто-реконнект)
    loop {
        info!("\n[STATUS] Сканирование эфира. Ожидание цели...");
        let _ = central.start_scan(ScanFilter::default()).await;

        let mut target_device = None;

        // Внутренний цикл: ждем, пока устройство не появится в зоне видимости
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
        info!("Цель обнаружена. Попытка захвата...");

        // Любая ошибка на этапе коннекта просто кидает нас в начало главного цикла
        if let Err(e) = device.connect().await {
            error!("Сбой подключения: {:?}. Повторная попытка...", e);
            continue;
        }

        if let Err(e) = device.discover_services().await {
            error!("Ошибка опроса сервисов: {:?}", e);
            let _ = device.disconnect().await;
            continue;
        }
        
        let chars = device.characteristics();
        let hr_char_opt = chars.iter().find(|c| c.uuid.to_string() == HR_CHARACTERISTIC);
        
        let hr_char = match hr_char_opt {
            Some(c) => c,
            None => {
                error!("Требуемая характеристика не найдена на устройстве.");
                let _ = device.disconnect().await;
                continue;
            }
        };

        if let Err(e) = device.subscribe(hr_char).await {
            error!("Ошибка подписки на поток: {:?}", e);
            let _ = device.disconnect().await;
            continue;
        }

        info!("[STATUS] Захват успешен. Телеметрия пишется в лог...");

        // Открываем файл для дозаписи (или создаем новый)
        // Проверяем текущий размер файла (если файла нет, размер считаем равным 0)
        let file_size = std::fs::metadata(LOG_FILE).map(|m| m.len()).unwrap_or(0);
        
        let mut opts = OpenOptions::new();
        opts.create(true).write(true);

        if file_size >= MAX_LOG_SIZE {
            info!("[STATUS] Лог достиг предела в {} байт. Файл будет перезаписан.", MAX_LOG_SIZE);
            opts.truncate(true);
        } else {
            opts.append(true);
        }

        let mut file = match opts.open(LOG_FILE) {
            Ok(f) => f,
            Err(e) => {
                error!("Ошибка доступа к файлу лога: {}", e);
                let _ = device.disconnect().await;
                continue;
            }
        };

        let mut notification_stream = match device.notifications().await {
            Ok(stream) => stream,
            Err(_) => {
                let _ = device.disconnect().await;
                continue;
            }
        };

        // Внутренний цикл чтения потока со сторожевым таймером
        loop {
            // Ждем максимум 5 секунд следующего пакета
            match timeout(Duration::from_secs(5), notification_stream.next()).await {
                Ok(Some(data)) => {
                    // Штатный режим: данные пришли вовремя
                    if data.uuid.to_string() == HR_CHARACTERISTIC {
                        if let Some(hr_data) = hrm::parse_payload(&data.value) {
                            
                            let timestamp = chrono::Utc::now().to_rfc3339();
                            let rr_str = format!("{:?}", hr_data.rr_intervals);
                            let csv_line = format!("{},{},{}\n", timestamp, hr_data.bpm, rr_str);
                            
                            print!("LIVE: {} BPM\r", hr_data.bpm);
                            use std::io::Write as _;
                            let _ = std::io::stdout().flush();
                            
                            if let Err(e) = file.write_all(csv_line.as_bytes()) {
                                error!("\nОшибка записи на диск: {}", e);
                            }
                        }
                    }
                }
                Ok(None) => {
                    // Поток закрылся штатно
                    info!("\n[STATUS] Поток данных закрыт устройством.");
                    break;
                }
                Err(_) => {
                    // Сработал таймаут (прошло 5 секунд без пакетов)
                    error!("\n[STATUS] Таймаут: нет данных 5 секунд. Возможен обрыв связи.");
                    break;
                }
            }
        }

        // Если мы выпали из внутреннего цикла, значит поток иссяк
        info!("\n[STATUS] Сброс мертвых соединений...");
        let _ = device.disconnect().await;
    }
}