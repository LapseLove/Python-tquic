// Copyright (c) 2023 The TQUIC Authors.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use log::*;

use super::CongestionController;
use super::CongestionStats;
use super::HystartPlusPlus;
use crate::congestion_control::pacing;
use crate::connection::rtt::RttEstimator;
use crate::connection::space::SentPacket;
use crate::RecoveryConfig;
use crate::frame;

#[derive(Debug)]
pub struct SQPConfig {
    /// Initail Pacing rate in bytes per second
    init_pacing_rate: u64,

    /// Frame Per Second
    frame_interval: u64,

    // Pacing multiplier, Default 2.0
    param_m: f64,

    // Target multiplier, Default 0.9
    param_t: f64,

    // Step size, Default 320 kbps
    param_delta: u64,

    // Reward weight, Default 0.25
    param_r: f64,
}

impl SQPConfig {
    pub fn new() -> Self {
        Self {
            init_pacing_rate: 2000000,
            frame_interval: 30,
            param_m: 2.0,
            param_t: 0.9,
            param_delta: 320000,
            param_r: 0.25,
        }
    }

    pub fn from(conf: &RecoveryConfig) -> Self {
        Self {
            init_pacing_rate: conf.sqp_init_pacing_rate,
            frame_interval: conf.sqp_frame_interval,
            param_m: conf.sqp_param_m,
            param_t: conf.sqp_param_t,
            param_delta: conf.sqp_param_delta,
            param_r: conf.sqp_param_r,
        }
    }

    pub fn set_frame_interval(&mut self, interval: u64) {
        self.frame_interval = interval;
    }
}

impl Default for SQPConfig {
    fn default() -> Self {
        Self {
            init_pacing_rate: 2000000,
            frame_interval: 30,
            param_m: 2.0,
            param_t: 0.9,
            param_delta: 320000,
            param_r: 0.25,
        }
    }
}

#[derive(Debug)]
pub struct SQP {
    /// Configurable parameters.
    config: SQPConfig,

    /// Statistics.
    stats: CongestionStats,

    min_rtt_filter: MinRttFilter,

    sent_bytes_in_total: u64,

    first_packet_sent_time_queue: VecDeque<Instant>,

    /// Time when the first packet is sent, refer to S_start in the paper
    first_packet_sent_time: Option<Instant>,

    /// Time when the last packet is acked, refer to R_end in the paper
    last_packet_acked_time: Option<Instant>,

    /// Time when the first packet is acked
    first_packet_acked_time: Option<Instant>,

    /// Total acked bytes
    acked_bytes_in_total: u64,

    /// Current frame size in bytes
    frame_size: VecDeque<u64>,

    /// Time when the current frame started
    frame_start_time: Option<Instant>,

    /// Current pacing rate in bit per second
    pacing_rate: u64,

    /// Current estimated bandwidth in bit per second
    bandwidth_estimate: u64,
}

impl SQP {
    pub fn new(conf: SQPConfig) -> Self {
        let pacing_rate = conf.init_pacing_rate;
        let bandwidth_estimate = pacing_rate / 2;
        Self {
            config: conf,
            stats: CongestionStats::default(),
            min_rtt_filter: MinRttFilter::new(),
            sent_bytes_in_total: 0,
            first_packet_sent_time_queue: VecDeque::new(),
            first_packet_sent_time: None,
            last_packet_acked_time: None,
            first_packet_acked_time: None,
            acked_bytes_in_total: 0,
            frame_size: VecDeque::new(),
            frame_start_time: None,
            pacing_rate: pacing_rate,
            bandwidth_estimate: bandwidth_estimate,
        }
    }
}

impl CongestionController for SQP {
    fn name(&self) -> &str {
        "SQP"
    }

    fn on_sent(&mut self, now: Instant, packet: &mut SentPacket, bytes_in_flight: u64) {
        self.stats.bytes_in_flight = bytes_in_flight;
        self.stats.bytes_sent_in_total = self
            .stats
            .bytes_sent_in_total
            .saturating_add(packet.sent_size as u64);

        self.sent_bytes_in_total = self
            .sent_bytes_in_total
            .saturating_add(packet.sent_size as u64);

        if self.first_packet_sent_time.is_none() && !self.frame_size.is_empty()
        {
            if !self.first_packet_sent_time_queue.is_empty() {
                self.first_packet_sent_time = Some(self.first_packet_sent_time_queue.pop_front().unwrap());
            }
            else {
                self.first_packet_sent_time = Some(packet.time_sent);
            }
        }

        // Buffer early sent frame time
        if self.first_packet_sent_time.is_some() && !self.frame_size.is_empty() && self.sent_bytes_in_total >= *self.frame_size.front().unwrap() {
            self.first_packet_sent_time_queue.push_back(packet.time_sent);
            self.sent_bytes_in_total = self.sent_bytes_in_total.saturating_sub(*self.frame_size.front().unwrap());
        }

    }

    fn begin_ack(&mut self, now: Instant, bytes_in_flight: u64) {
        
    }

    fn on_ack(
        &mut self,
        packet: &mut SentPacket,
        now: Instant,
        app_limited: bool,
        rtt: &RttEstimator,
        bytes_in_flight: u64,
    ) {
        let acked_bytes = packet.sent_size as u64;
        self.stats.bytes_in_flight = self.stats.bytes_in_flight.saturating_sub(acked_bytes);
        self.stats.bytes_acked_in_total = self.stats.bytes_acked_in_total.saturating_add(acked_bytes);
        if self.in_slow_start() {
            self.stats.bytes_acked_in_slow_start = self
                .stats
                .bytes_acked_in_slow_start
                .saturating_add(acked_bytes);
        }

        // Update min_RTT
        self.min_rtt_filter.set_window(rtt.smoothed_rtt() * 2);
        self.min_rtt_filter.update(rtt.latest_rtt().as_nanos(), now);

        self.last_packet_acked_time = Some(now);

        // Update Bandwidth Sampling and Pacing Rate
        if !self.frame_size.is_empty() {
            let frame_size = self.frame_size.front().unwrap();
            // println!("self.frame_size: {}", frame_size);
            if let Some(start_send_time) = self.first_packet_sent_time {
                if start_send_time <= packet.time_sent {
                    if self.acked_bytes_in_total == 0 {
                        self.first_packet_acked_time = Some(now);
                    }
                    self.acked_bytes_in_total = self.acked_bytes_in_total.saturating_add(acked_bytes);
                }
            }
            if self.acked_bytes_in_total >= *frame_size {
                let f_max = self.bandwidth_estimate as f64 * (1.0 / self.config.frame_interval as f64);
                let gamma = f_max / (*frame_size as f64 * 8.0);
                let r = self.last_packet_acked_time.unwrap().saturating_duration_since(self.first_packet_sent_time.unwrap()).as_secs_f64() / 2.0;
                let rr = self.last_packet_acked_time.unwrap().saturating_duration_since(self.first_packet_acked_time.unwrap()).as_secs_f64();
                let delta_min = self.min_rtt_filter.get_min_rtt().unwrap() as f64 / 2_000_000_000.0;
                let delta = r - delta_min as f64  + rr * (gamma - 1.0);
                let sample = (*frame_size as f64 * gamma * 8.0) / delta;
                let b_sample = self.config.param_t * sample;
                println!("--- SQP Bandwidth Sample Info ---");
                // println!("config.param_t: {}, config.param_m: {}, config.param_delta: {}, config.param_r: {}", self.config.param_t, self.config.param_m, self.config.param_delta, self.config.param_r);
                println!("SQP Sample: {}, f_max: {}, gamma: {}, r: {}, delta_min: {}, delta: {}, rr: {}", b_sample, f_max, gamma, r, delta_min, delta, rr);
                // Update estimated bandwidth
                let estimated_bandwidth = (self.bandwidth_estimate as f64 + self.config.param_delta as f64 * 
                (self.config.param_r * (b_sample / self.bandwidth_estimate as f64 - 1.0) - (self.bandwidth_estimate as f64 / b_sample - 1.0))) as u64;
                if estimated_bandwidth != 0 {
                    self.bandwidth_estimate = estimated_bandwidth;
                    self.pacing_rate = (self.bandwidth_estimate as f64 * self.config.param_m / 8.0) as u64;
                }
                self.acked_bytes_in_total = 0;
                self.frame_size.pop_front();
                self.first_packet_sent_time = None;
                self.first_packet_acked_time = None;
                println!("SQP Estimated Bandwidth: {}, Self Bandwidth: {}", estimated_bandwidth, self.bandwidth_estimate);
                println!("--- SQP Bandwidth Sample End ---");
            }
        }
    }

    fn end_ack(&mut self) {
        
    }

    fn on_congestion_event(
        &mut self,
        now: Instant,
        packet: &SentPacket,
        is_persistent_congestion: bool,
        lost_bytes: u64,
        bytes_in_flight: u64,
    ) {
        // Statistics.
        self.stats.bytes_lost_in_total = self.stats.bytes_lost_in_total.saturating_add(lost_bytes);
        self.stats.bytes_in_flight = bytes_in_flight;

        if self.in_slow_start() {
            self.stats.bytes_lost_in_slow_start = self
                .stats
                .bytes_lost_in_slow_start
                .saturating_add(lost_bytes);
        }
    }

    fn in_slow_start(&self) -> bool {
        false
    }

    fn in_recovery(&self, sent_time: Instant) -> bool {
        false
    }

    fn congestion_window(&self) -> u64 {
        10000000
    }

    fn initial_window(&self) -> u64 {
        10000000
    }

    fn minimal_window(&self) -> u64 {
        10000000
    }

    fn stats(&self) -> &CongestionStats {
        &self.stats
    }

    fn pacing_rate(&self) -> Option<u64> {
        Some(self.pacing_rate)
    }

    fn min_pacing_rate(&self) -> Option<u64> {
        Some(self.pacing_rate)
    }

    fn set_sqp_frame_size(&mut self, size: usize) {
        self.frame_size.push_back(size as u64);
    }

    fn get_sqp_bandwidth_estimate(&self) -> Option<u64> {
        Some(self.bandwidth_estimate)
    }
}

#[derive(Debug)]
struct MinRttFilter {
    min_rtt_queue: VecDeque<(u128, Instant)>,
    window: Duration,
}

impl MinRttFilter {
    pub fn new() -> Self {
        Self {
            min_rtt_queue: VecDeque::new(),
            window: Duration::from_secs(1),
        }
    }

    pub fn update(&mut self, rtt: u128, now: Instant) {
        self.min_rtt_queue.push_back((rtt, now));
        while let Some((_, time)) = self.min_rtt_queue.front() {
            if now.duration_since(*time) > self.window {
                self.min_rtt_queue.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn get_min_rtt(&self) -> Option<u128> {
        self.min_rtt_queue.iter().map(|(rtt, _)| *rtt).min()
    }

    pub fn set_window(&mut self, window: Duration) {
        self.window = window;
    }
}

#[cfg(test)]
mod tests {

}