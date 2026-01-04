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

use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use log::*;

use super::CongestionController;
use super::CongestionStats;
use super::HystartPlusPlus;
use crate::connection::rtt::RttEstimator;
use crate::connection::space::SentPacket;
use crate::RecoveryConfig;
use crate::congestion_control::bbr;
use crate::congestion_control::copa;
use crate::congestion_control::cubic;

// #[derive(Debug)]
// pub struct MixCCConfig {

// }

// impl MixCCConfig {
  
// }

// impl Default for MixCCConfig {
    
// }

#[derive(Debug)]
pub struct MixCC {
    bbr: bbr::Bbr,
    cubic: cubic::Cubic,
    copa: copa::Copa,
}

impl MixCC {
    pub fn new() -> Self {
        Self {
            bbr: bbr::Bbr::new(bbr::BbrConfig::default()),
            cubic: cubic::Cubic::new(cubic::CubicConfig::default()),
            copa: copa::Copa::new(copa::CopaConfig::default()),
        }
    }
}

impl CongestionController for MixCC {
    fn name(&self) -> &str {
        "MixCC"
    }

    fn on_sent(&mut self, now: Instant, packet: &mut SentPacket, bytes_in_flight: u64) {
        self.bbr.on_sent(now, packet, bytes_in_flight);
        println!("MixCC on_sent: BBR");
        self.copa.on_sent(now, packet, bytes_in_flight);
        println!("MixCC on_sent: COPA");
        self.cubic.on_sent(now, packet, bytes_in_flight);
        println!("MixCC on_sent: CUBIC");
    }

    fn begin_ack(&mut self, now: Instant, bytes_in_flight: u64) {
        self.bbr.begin_ack(now, bytes_in_flight);
        self.copa.begin_ack(now, bytes_in_flight);
        self.cubic.begin_ack(now, bytes_in_flight);
    }

    fn on_ack(
        &mut self,
        packet: &mut SentPacket,
        now: Instant,
        app_limited: bool,
        rtt: &RttEstimator,
        bytes_in_flight: u64,
    ) {
        self.bbr.on_ack(packet, now, app_limited, rtt, bytes_in_flight);
        self.copa.on_ack(packet, now, app_limited, rtt, bytes_in_flight);
        self.cubic.on_ack(packet, now, app_limited, rtt, bytes_in_flight);
    }

    fn end_ack(&mut self) {
        self.bbr.end_ack();
        self.copa.end_ack();
        self.cubic.end_ack();
    }

    fn on_congestion_event(
        &mut self,
        now: Instant,
        packet: &SentPacket,
        is_persistent_congestion: bool,
        lost_bytes: u64,
        bytes_in_flight: u64,
    ) {
        self.bbr.on_congestion_event(now, packet, is_persistent_congestion, lost_bytes, bytes_in_flight);
        self.copa.on_congestion_event(now, packet, is_persistent_congestion, lost_bytes, bytes_in_flight);
        self.cubic.on_congestion_event(now, packet, is_persistent_congestion, lost_bytes, bytes_in_flight);
    }

    fn in_slow_start(&self) -> bool {
        let bbr_s = self.bbr.in_slow_start();
        let copa_s = self.copa.in_slow_start();
        let cubic_s = self.cubic.in_slow_start();
        bbr_s && copa_s && cubic_s
    }

    fn in_recovery(&self, sent_time: Instant) -> bool {
        let bbr_r = self.bbr.in_recovery(sent_time);
        let copa_r = self.copa.in_recovery(sent_time);
        let cubic_r = self.cubic.in_recovery(sent_time);
        bbr_r || copa_r || cubic_r
    }

    fn congestion_window(&self) -> u64 {
        let bbr_c = self.bbr.congestion_window();
        let copa_c = self.cubic.congestion_window();
        let cubic_c = self.cubic.congestion_window();
        std::cmp::min(bbr_c, std::cmp::min(copa_c, cubic_c))
    }

    fn initial_window(&self) -> u64 {
        let bbr_iw = self.bbr.initial_window();
        let copa_iw = self.cubic.initial_window();
        let cubic_iw = self.cubic.initial_window();
        std::cmp::min(bbr_iw, std::cmp::min(copa_iw, cubic_iw))
    }

    fn minimal_window(&self) -> u64 {
        let bbr_mw = self.bbr.minimal_window();
        let copa_mw = self.cubic.minimal_window();
        let cubic_mw = self.cubic.minimal_window();
        std::cmp::min(bbr_mw, std::cmp::min(copa_mw, cubic_mw))
    }

    fn stats(&self) -> &CongestionStats {
        self.bbr.stats()
    }

    fn pacing_rate(&self) -> Option<u64> {
        let bbr_pr = self.bbr.pacing_rate();
        let copa_pr = self.cubic.pacing_rate();
        let cubic_pr = self.cubic.pacing_rate();
        match (bbr_pr, copa_pr, cubic_pr) {
            (Some(bbr_rate), Some(copa_rate), Some(cubic_rate)) => {
                Some(std::cmp::min(bbr_rate, std::cmp::min(copa_rate, cubic_rate)))
            }
            (Some(bbr_rate), Some(copa_rate), None) => Some(std::cmp::min(bbr_rate, copa_rate)),
            (Some(bbr_rate), None, Some(cubic_rate)) => Some(std::cmp::min(bbr_rate, cubic_rate)),
            (None, Some(copa_rate), Some(cubic_rate)) => Some(std::cmp::min(copa_rate, cubic_rate)),
            (Some(bbr_rate), None, None) => Some(bbr_rate),
            (None, Some(copa_rate), None) => Some(copa_rate),
            (None, None, Some(cubic_rate)) => Some(cubic_rate),
            (None, None, None) => None,
        }
    }

    fn min_pacing_rate(&self) -> Option<u64> {
        let bbr_mpr = self.bbr.min_pacing_rate();
        let copa_mpr = self.cubic.min_pacing_rate();
        let cubic_mpr = self.cubic.min_pacing_rate();
        match (bbr_mpr, copa_mpr, cubic_mpr) {
            (Some(bbr_rate), Some(copa_rate), Some(cubic_rate)) => {
                Some(std::cmp::max(bbr_rate, std::cmp::max(copa_rate, cubic_rate)))
            }
            (Some(bbr_rate), Some(copa_rate), None) => Some(std::cmp::max(bbr_rate, copa_rate)),
            (Some(bbr_rate), None, Some(cubic_rate)) => Some(std::cmp::max(bbr_rate, cubic_rate)),
            (None, Some(copa_rate), Some(cubic_rate)) => Some(std::cmp::max(copa_rate, cubic_rate)),
            (Some(bbr_rate), None, None) => Some(bbr_rate),
            (None, Some(copa_rate), None) => Some(copa_rate),
            (None, None, Some(cubic_rate)) => Some(cubic_rate),
            (None, None, None) => None,
        }
    }
}

#[cfg(test)]
mod tests {

}