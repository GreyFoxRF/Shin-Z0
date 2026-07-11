use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::Manager;
use std::time::Duration;
const TARGET_MAC: &str = "D7:3B:5E:CF:DA:3F";

#[tokio::main]
async fn main() {
    println!("Shin-Z0: System initialized. Ready to search");

    // Инициализируем менеджер Bluetooth (точка входа в API ОС)
    let manager = Manager::new().await.unwrap();

    // Запрашиваем список всех Bluetooth адаптеров на твоем компьютере
    let adapters = manager.adapters().await.unwrap();

    // Проверяем, нашел ли он хоть один адаптер (вдруг у тебя на Linux выключен Bluetooth)
    if adapters.is_empty() {
        eprintln!("Bluetooth адаптер не найден. Включи Bluetooth!");
        return;
    }

    // Берем первый попавшийся адаптер (обычно он один)
    let central = &adapters[0];
    
    println!("Using an adapter: {:?}", central);

    central.start_scan(ScanFilter::default()).await.unwrap();
    tokio::time::sleep(Duration::from_secs(4)).await;
    let peripherals = central.peripherals().await.unwrap();

    // 1. Создаем изменяемую переменную типа Option. 
    // Пока мы ничего не нашли, внутри лежит None (ничто).
    let mut target_device = None;

    // 2. Фаза разведки. Перебираем эфир.
    for p in peripherals {
        if let Some(props) = p.properties().await.unwrap() {
            if props.address.to_string() == TARGET_MAC {
                println!("Найден целевой пульсометр: {:?}", props.local_name);
                // Помещаем найденное устройство в "коробку" Some и выходим из цикла
                target_device = Some(p);
                break;
            }
        }
    }

    // 3. Извлекаем устройство.
    // expect — это жесткий метод распаковки. Если внутри target_device лежит None
    // (пульсометр не включен), программа упадет и выдаст твой текст ошибки. 
    // Если там Some(device), она достанет устройство и положит его в переменную device.
    let device = target_device.expect("Пульсометр не найден в эфире. Надень его!");

    // 4. Фаза работы. Подключаемся уже вне цикла сканирования.
    device.connect().await.unwrap();
    println!("Успешное подключение к устройству.");

    // 5. Опрашиваем внутренности (то, что мы обсуждали на прошлом шаге)
    device.discover_services().await.unwrap();
    
    for c in device.characteristics() {
        println!("Найдена характеристика: {}", c.uuid);
    }


}


