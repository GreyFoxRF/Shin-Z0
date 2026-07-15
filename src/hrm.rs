use chrono::{Datelike, TimeZone, Utc};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Telemetry {
    pub bpm: u16,
    pub rr_ms: Vec<u16>,
    pub age: u8,
    pub hr_zone: String,
    pub rmssd: f32,
    pub rmssd_status: String,
    pub sdnn: f32,
    pub baevsky_index: f32,
    pub stress_status: String,
    pub resp_rate: f32,
}

pub struct BioAnalyzer {
    rr_history: Vec<u16>,
    window_size: usize,
}

impl Default for BioAnalyzer {
    fn default() -> Self {
        Self::new(60) // Храним последние 60 ударов для статистики
    }
}

impl BioAnalyzer {
    pub fn new(window_size: usize) -> Self {
        Self {
            rr_history: Vec::with_capacity(window_size),
            window_size,
        }
    }

    fn current_age(&self) -> u8 {
        let dob = Utc.with_ymd_and_hms(1988, 5, 26, 0, 0, 0).unwrap();
        let now = Utc::now();
        let mut age = now.year() - dob.year();
        if now.month() < dob.month() || (now.month() == dob.month() && now.day() < dob.day()) {
            age -= 1;
        }
        age as u8
    }

    pub fn process_payload(&mut self, bytes: &[u8]) -> Option<Telemetry> {
        if bytes.is_empty() { return None; }

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
                
                self.rr_history.push(rr_ms);
                if self.rr_history.len() > self.window_size {
                    self.rr_history.remove(0);
                }
                index += 2;
            }
        }

        let age = self.current_age();
        let max_hr = 220.0 - age as f32;
        let hr_percent = (bpm as f32 / max_hr) * 100.0;
        
        let hr_zone = match hr_percent {
            p if p >= 90.0 => "ZONE 5 [REDLINE]".to_string(),
            p if p >= 80.0 => "ZONE 4 [ANAEROBIC]".to_string(),
            p if p >= 70.0 => "ZONE 3 [AEROBIC]".to_string(),
            p if p >= 60.0 => "ZONE 2 [FAT BURN]".to_string(),
            _ => "ZONE 1 [REST/WARMUP]".to_string(),
        };

        let rmssd = self.calc_rmssd();
        let rmssd_status = match rmssd {
            r if r < 20.0 => "CRITICAL [SYMPATHETIC DOMINANCE]".to_string(),
            r if r < 25.0 => "WARNING [FATIGUE]".to_string(),
            r if r < 45.0 => "OPTIMAL [BALANCED]".to_string(),
            _ => "EXCELLENT [PARASYMPATHETIC]".to_string(),
        };

        let sdnn = self.calc_sdnn();
        let baevsky_index = self.calc_baevsky();
        
        let stress_status = match baevsky_index {
            b if b > 200.0 => "OVERFATIGUE [CRITICAL LOAD]".to_string(),
            b if b > 150.0 => "HIGH STRESS [LIMITING]".to_string(),
            b if b > 50.0 => "NORMAL ADAPTATION".to_string(),
            b if b > 0.0 => "RELAXED [LOW TENSION]".to_string(),
            _ => "CALCULATING...".to_string(),
        };

        let resp_rate = if bpm > 0 { 60000.0 / self.calc_mean_rr() } else { 0.0 };

        Some(Telemetry {
            bpm,
            rr_ms: current_rr,
            age,
            hr_zone,
            rmssd,
            rmssd_status,
            sdnn,
            baevsky_index,
            stress_status,
            resp_rate: resp_rate / 4.0, // Упрощенная аппроксимация RSA
        })
    }

    fn calc_rmssd(&self) -> f32 {
        if self.rr_history.len() < 2 { return 0.0; }
        let mut sum_sq = 0.0;
        for i in 0..(self.rr_history.len() - 1) {
            let diff = self.rr_history[i + 1] as f32 - self.rr_history[i] as f32;
            sum_sq += diff * diff;
        }
        (sum_sq / (self.rr_history.len() - 1) as f32).sqrt()
    }

    fn calc_mean_rr(&self) -> f32 {
        if self.rr_history.is_empty() { return 800.0; }
        let sum: u32 = self.rr_history.iter().map(|&x| x as u32).sum();
        sum as f32 / self.rr_history.len() as f32
    }

    fn calc_sdnn(&self) -> f32 {
        if self.rr_history.len() < 2 { return 0.0; }
        let mean = self.calc_mean_rr();
        let mut sum_sq = 0.0;
        for &rr in &self.rr_history {
            let diff = rr as f32 - mean;
            sum_sq += diff * diff;
        }
        (sum_sq / (self.rr_history.len() - 1) as f32).sqrt()
    }

    fn calc_baevsky(&self) -> f32 {
        if self.rr_history.len() < 20 { return 0.0; }
        let min_rr = *self.rr_history.iter().min().unwrap_or(&0) as f32 / 1000.0;
        let max_rr = *self.rr_history.iter().max().unwrap_or(&0) as f32 / 1000.0;
        let delta_x = max_rr - min_rr;
        if delta_x <= 0.0 { return 0.0; }

        let mut bins = HashMap::new();
        for &rr in &self.rr_history {
            let bin = (rr / 50) * 50; 
            *bins.entry(bin).or_insert(0) += 1;
        }
        
        let (mode_bin, max_count) = bins.into_iter().max_by_key(|&(_, count)| count).unwrap();
        let mode = mode_bin as f32 / 1000.0;
        let amo = (max_count as f32 / self.rr_history.len() as f32) * 100.0;

        if mode > 0.0 { amo / (2.0 * mode * delta_x) } else { 0.0 }
    }
}