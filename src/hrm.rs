#[derive(Debug)]
pub struct HeartRateData {
    pub bpm: u16,
    pub rr_intervals: Vec<u16>,
}

pub fn parse_payload(bytes: &[u8]) -> Option<HeartRateData> {
    if bytes.is_empty() {
        return None;
    }

    let flags = bytes[0];
    let is_16bit = (flags & 0x01) != 0;
    let rr_present = (flags & 0x10) != 0;

    let mut index = 1;

    let bpm: u16 = if is_16bit {
        if bytes.len() < 3 { return None; }
        let val = (bytes[index] as u16) | ((bytes[index + 1] as u16) << 8);
        index += 2;
        val
    } else {
        if bytes.len() < 2 { return None; }
        let val = bytes[index] as u16;
        index += 1;
        val
    };

    let mut rr_intervals = Vec::new();
    if rr_present {
        while index + 1 < bytes.len() {
            let raw_rr = (bytes[index] as u16) | ((bytes[index + 1] as u16) << 8);
            let rr_ms = (raw_rr as f32 * 1000.0 / 1024.0).round() as u16;
            rr_intervals.push(rr_ms);
            index += 2;
        }
    }

    Some(HeartRateData { bpm, rr_intervals })
}