mod hrm;

use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::Manager;
use std::time::Duration;
use futures::stream::StreamExt;

const HR_CHARACTERISTIC: &str = "00002a37-0000-1000-8000-00805f9b34fb";
const TARGET_MAC: &str = "D7:3B:5E:CF:DA:3F";

#[tokio::main]
async fn main() {
    println!("--> Shin-Z0: System initialized. Ready to search.");

    let manager = Manager::new().await.unwrap();
    let adapters = manager.adapters().await.unwrap();

    if adapters.is_empty() {
        eprintln!("--> Bluetooth adapter not found. Turn it on.");
        return;
    }

    let central = &adapters[0];
    println!("--> Using adapter: {:?}", central);

    central.start_scan(ScanFilter::default()).await.unwrap();
    tokio::time::sleep(Duration::from_secs(4)).await;
    let peripherals = central.peripherals().await.unwrap();

    let mut target_device = None;

    for p in peripherals {
        if let Some(props) = p.properties().await.unwrap() {
            if props.address.to_string() == TARGET_MAC {
                println!("--> Found target heart rate monitor: {:?}", props.local_name);
                target_device = Some(p);
                break;
            }
        }
    }

    let device = target_device.expect("--> HRM not found in the air. Put it on.");

    device.connect().await.unwrap();
    println!("--> Connection established.");

    device.discover_services().await.unwrap();
    
    let chars = device.characteristics();
    let hr_char = chars.iter().find(|c| c.uuid.to_string() == HR_CHARACTERISTIC)
        .expect("--> Pulse characteristic not found on the device.");

    println!("--> Subscribing to Pulse updates...");
    device.subscribe(hr_char).await.unwrap();
    println!("--> Subscription activated. Waiting for data...");

    let mut notification_stream = device.notifications().await.unwrap();

    while let Some(data) = notification_stream.next().await {
        if data.uuid.to_string() == HR_CHARACTERISTIC {
            if let Some(hr_data) = hrm::parse_payload(&data.value) {
                println!("--> Пульс: {} BPM | RR-интервалы: {:?} мс", hr_data.bpm, hr_data.rr_intervals);
            }
        }
    }
}