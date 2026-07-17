use std::collections::HashMap;
use serde::{Serialize, Deserialize};
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileMetrics {
    pub rmssd: f32,
    pub rmssd_status: String,
    pub sdnn: f32,
    pub sdnn_status: String,
    pub baevsky_index: f32,
    pub stress_status: String,
    pub pnn50: f32,
    pub pnn50_status: String,
    pub cv: f32,
    pub cv_status: String,
    pub lf_hf_ratio: f32, 
    pub dfa_a1: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Telemetry {
    pub bpm: u16,
    pub rr_ms: Vec<u16>,
    pub age: u8,
    pub hr_zone: String,
    pub resp_rate: f32,
    pub anomalies: usize,
    pub operative: ProfileMetrics,
    pub baseline: ProfileMetrics, 
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonPayload {
    pub daemon_state: String,
    pub sys_msg: String,
    pub telemetry: Option<Telemetry>,
}

pub struct BioAnalyzer {
    master_buffer: Vec<u16>,
    last_update: Option<Instant>,
}

impl Default for BioAnalyzer {
    fn default() -> Self {
        Self { 
            master_buffer: Vec::with_capacity(1000),
            last_update: None,
        }
    }
}

impl BioAnalyzer {
    pub fn clear(&mut self) {
        self.master_buffer.clear();
        self.last_update = None;
    }

    pub fn process_payload(&mut self, bytes: &[u8], subject_age: u8) -> Option<Telemetry> {
        if bytes.is_empty() { return None; }

        if let Some(last) = self.last_update {
            if last.elapsed().as_secs() >= 5 {
                self.master_buffer.clear();
            }
        }
        self.last_update = Some(Instant::now());

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

        let mut current_rr = Vec::new();
        if rr_present {
            while index + 1 < bytes.len() {
                let raw_rr = (bytes[index] as u16) | ((bytes[index + 1] as u16) << 8);
                let rr_ms = (raw_rr as f32 * 1000.0 / 1024.0).round() as u16;
                current_rr.push(rr_ms);
                
                self.master_buffer.push(rr_ms);
                if self.master_buffer.len() > 1000 {
                    self.master_buffer.remove(0);
                }
                index += 2;
            }
        }

        let max_hr = 220.0 - subject_age as f32;
        let hr_percent = (bpm as f32 / max_hr) * 100.0;
        
        let hr_zone = match hr_percent {
            p if p >= 90.0 => "ZONE 5 [REDLINE]".to_string(),
            p if p >= 80.0 => "ZONE 4 [ANAEROBIC]".to_string(),
            p if p >= 70.0 => "ZONE 3 [AEROBIC]".to_string(),
            p if p >= 60.0 => "ZONE 2 [FAT BURN]".to_string(),
            _ => "ZONE 1 [REST/WARMUP]".to_string(),
        };

        let op_len = std::cmp::min(self.master_buffer.len(), 60);
        let op_slice = &self.master_buffer[self.master_buffer.len() - op_len..];
        let operative = self.calc_profile(op_slice);
        
        let base_len = std::cmp::min(self.master_buffer.len(), 300);
        let base_slice = &self.master_buffer[self.master_buffer.len() - base_len..];
        let baseline = self.calc_profile(base_slice);

        let anomalies = self.detect_anomalies(op_slice);
        let mean_rr = if self.master_buffer.is_empty() { 800.0 } else { 
            self.master_buffer.iter().map(|&x| x as f32).sum::<f32>() / self.master_buffer.len() as f32 
        };
        let resp_rate = if bpm > 0 { 60000.0 / mean_rr } else { 0.0 };

        Some(Telemetry {
            bpm, rr_ms: current_rr, age: subject_age,
            hr_zone, resp_rate: resp_rate / 4.0, anomalies,
            operative, baseline,
        })
    }

    fn calc_profile(&self, slice: &[u16]) -> ProfileMetrics {
        if slice.len() < 2 {
            return ProfileMetrics {
                rmssd: 0.0, rmssd_status: "AWAITING...".into(),
                sdnn: 0.0, sdnn_status: "AWAITING...".into(),
                baevsky_index: 0.0, stress_status: "AWAITING...".into(),
                pnn50: 0.0, pnn50_status: "AWAITING...".into(),
                cv: 0.0, cv_status: "AWAITING...".into(),
                lf_hf_ratio: 0.0, dfa_a1: 0.0,
            };
        }

        let mut sum_sq_diff = 0.0;
        let mut nn50_count = 0;
        let mean_rr = slice.iter().map(|&x| x as f32).sum::<f32>() / slice.len() as f32;
        let mut sum_sq_dev = 0.0;

        for i in 0..(slice.len() - 1) {
            let diff = (slice[i + 1] as f32 - slice[i] as f32).abs();
            sum_sq_diff += diff * diff;
            if diff > 50.0 { nn50_count += 1; }
        }

        for &rr in slice {
            let dev = rr as f32 - mean_rr;
            sum_sq_dev += dev * dev;
        }

        let count = (slice.len() - 1) as f32;
        let rmssd = (sum_sq_diff / count).sqrt();
        let sdnn = (sum_sq_dev / count).sqrt();
        let pnn50 = (nn50_count as f32 / count) * 100.0;
        let cv = if mean_rr > 0.0 { (sdnn / mean_rr) * 100.0 } else { 0.0 };

        let mut baevsky_index = 0.0;
        if slice.len() >= 20 {
            let min_rr = *slice.iter().min().unwrap_or(&0) as f32 / 1000.0;
            let max_rr = *slice.iter().max().unwrap_or(&0) as f32 / 1000.0;
            let delta_x = max_rr - min_rr;
            if delta_x > 0.0 {
                let mut bins = HashMap::new();
                for &rr in slice {
                    let bin = (rr / 50) * 50; 
                    *bins.entry(bin).or_insert(0) += 1;
                }
                let (mode_bin, max_count) = bins.into_iter().max_by_key(|&(_, count)| count).unwrap();
                let mode = mode_bin as f32 / 1000.0;
                let amo = (max_count as f32 / slice.len() as f32) * 100.0;
                if mode > 0.0 { baevsky_index = amo / (2.0 * mode * delta_x); }
            }
        }

        let rmssd_status = match rmssd {
            r if r < 20.0 => "CRITICAL".to_string(),
            r if r < 30.0 => "WARNING".to_string(),
            r if r < 50.0 => "OPTIMAL".to_string(),
            _ => "RELAXED".to_string(),
        };

        let sdnn_status = match sdnn {
            s if s < 30.0 => "RIGID".to_string(),
            s if s < 50.0 => "NORMAL".to_string(),
            _ => "HIGH".to_string(),
        };

        let stress_status = match baevsky_index {
            b if b > 200.0 => "OVERFATIGUE".to_string(),
            b if b > 150.0 => "HIGH STRESS".to_string(),
            b if b > 50.0  => "NORMAL ADAPT".to_string(),
            b if b > 0.0   => "RELAXED".to_string(),
            _ => "CALCULATING".to_string(),
        };

        let pnn50_status = match pnn50 {
            p if p < 3.0 => "RIGID".to_string(),
            p if p < 10.0 => "MODERATE".to_string(),
            _ => "FLEXIBLE".to_string(),
        };

        let cv_status = match cv {
            c if c < 2.0 => "RIGID".to_string(),
            c if c < 6.0 => "NORMAL".to_string(),
            _ => "CHAOTIC".to_string(),
        };

        ProfileMetrics { 
            rmssd, rmssd_status, sdnn, sdnn_status, 
            baevsky_index, stress_status, pnn50, pnn50_status, 
            cv, cv_status, lf_hf_ratio: 0.0, dfa_a1: 0.0, 
        }
    }

    fn detect_anomalies(&self, slice: &[u16]) -> usize {
        if slice.len() < 2 { return 0; }
        let mut anomalies = 0;
        for i in 0..(slice.len() - 1) {
            let diff = (slice[i + 1] as f32 - slice[i] as f32).abs();
            let prev = slice[i] as f32;
            if prev > 0.0 && (diff / prev) > 0.20 { anomalies += 1; }
        }
        anomalies
    }
}