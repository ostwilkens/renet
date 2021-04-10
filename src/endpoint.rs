use crate::packet::{FragmentHeader, HeaderParser, PacketHeader, PacketType};
use crate::reassembly_fragment::{build_fragments, FragmentConfig, ReassemblyFragment};
use crate::sequence_buffer::SequenceBuffer;
use crate::{
    error::{RenetError, Result},
    packet::HeartbeatHeader,
};

use log::{debug, error};
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct EndpointConfig {
    pub name: String,
    pub max_packet_size: usize,
    pub sent_packets_buffer_size: usize,
    pub received_packets_buffer_size: usize,
    pub measure_smoothing_factor: f64,
    pub timeout_duration: Duration,
    pub heartbeat_time: Duration,
    pub fragment_config: FragmentConfig,
}

impl Default for EndpointConfig {
    fn default() -> Self {
        EndpointConfig {
            name: "Endpoint".into(),
            max_packet_size: 16 * 1024,
            sent_packets_buffer_size: 256,
            received_packets_buffer_size: 256,
            measure_smoothing_factor: 0.05,
            timeout_duration: Duration::from_secs(5),
            heartbeat_time: Duration::from_millis(100),
            fragment_config: FragmentConfig::default(),
        }
    }
}

#[derive(Debug, Clone)]
struct SentPacket {
    time: Instant,
    ack: bool,
    /// Packet size in bytes
    size: usize,
}

impl Default for SentPacket {
    fn default() -> Self {
        Self {
            size: 0,
            ack: false,
            time: Instant::now(),
        }
    }
}

impl SentPacket {
    fn new(time: Instant, size: usize) -> Self {
        Self {
            time,
            size,
            ack: false,
        }
    }
}

#[derive(Debug, Clone)]
struct ReceivedPacket {
    time: Instant,
    /// Packet size in bytes
    size: usize,
}

impl Default for ReceivedPacket {
    fn default() -> Self {
        Self {
            size: 0,
            time: Instant::now(),
        }
    }
}

impl ReceivedPacket {
    fn new(time: Instant, size: usize) -> Self {
        Self { time, size }
    }
}

pub struct Endpoint {
    config: EndpointConfig,
    sequence: u16,
    reassembly_buffer: SequenceBuffer<ReassemblyFragment>,
    sent_buffer: SequenceBuffer<SentPacket>,
    received_buffer: SequenceBuffer<ReceivedPacket>,
    acks: Vec<u16>,
    network_info: NetworkInfo,
}

#[derive(Debug)]
pub struct NetworkInfo {
    pub rtt: f64,
    pub sent_bandwidth_kbps: f64,
    pub received_bandwidth_kbps: f64,
    pub packet_loss: f64,
}

impl Default for NetworkInfo {
    fn default() -> Self {
        Self {
            rtt: 0.,
            sent_bandwidth_kbps: 0.,
            received_bandwidth_kbps: 0.,
            packet_loss: 0.,
        }
    }
}

impl Endpoint {
    pub fn new(config: EndpointConfig) -> Self {
        Self {
            sequence: 0,
            reassembly_buffer: SequenceBuffer::with_capacity(
                config.fragment_config.reassembly_buffer_size,
            ),
            sent_buffer: SequenceBuffer::with_capacity(config.sent_packets_buffer_size),
            received_buffer: SequenceBuffer::with_capacity(config.received_packets_buffer_size),
            config,
            acks: vec![],
            network_info: NetworkInfo {
                rtt: 0.0,
                sent_bandwidth_kbps: 0.0,
                received_bandwidth_kbps: 0.0,
                packet_loss: 0.0,
            },
        }
    }

    pub fn config(&self) -> &EndpointConfig {
        &self.config
    }

    pub fn sequence(&self) -> u16 {
        self.sequence
    }

    pub fn network_info(&self) -> &NetworkInfo {
        &self.network_info
    }

    pub fn build_heartbeat_packet(&self) -> Result<Vec<u8>> {
        let (ack, ack_bits) = self.received_buffer.ack_bits();
        let header = HeartbeatHeader { ack, ack_bits };
        let mut buffer = vec![0u8; header.size()];
        header.write(&mut buffer)?;

        Ok(buffer)
    }

    pub fn generate_packets(&mut self, payload: &[u8]) -> Result<Vec<Vec<u8>>> {
        if payload.len() > self.config.max_packet_size {
            error!(
                "[{}] packet to large to send, maximum is {} got {}.",
                self.config.name,
                self.config.max_packet_size,
                payload.len()
            );
            return Err(RenetError::MaximumPacketSizeExceeded);
        }

        let sequence = self.sequence;
        self.sequence += 1;

        let (ack, ack_bits) = self.received_buffer.ack_bits();
        // TODO: add header size
        let sent_packet = SentPacket::new(Instant::now(), payload.len());
        self.sent_buffer.insert(sequence, sent_packet);
        if payload.len() > self.config.fragment_config.fragment_above {
            // Fragment packet
            debug!(
                "[{}] sending fragmented packet {}.",
                self.config.name, sequence
            );
            Ok(build_fragments(
                payload,
                sequence,
                ack,
                ack_bits,
                &self.config.fragment_config,
            )?)
        } else {
            // Normal packet
            debug!("[{}] sending normal packet {}.", self.config.name, sequence);
            let packet = build_normal_packet(payload, sequence, ack, ack_bits)?;
            Ok(vec![packet])
        }
    }

    pub fn send_to(&mut self, payload: &[u8], addrs: SocketAddr, socket: &UdpSocket) -> Result<()> {
        let packets = self.generate_packets(payload)?;
        for packet in packets {
            socket.send_to(&packet, addrs)?;
        }

        Ok(())
    }

    pub fn recv_from(
        &mut self,
        buf: &mut [u8],
        socket: &UdpSocket,
    ) -> Result<Option<(Vec<u8>, SocketAddr)>> {
        let (n, addrs) = socket.recv_from(buf)?;
        let payload = &mut buf[..n];
        if payload.len() > self.config.max_packet_size {
            error!(
                "[{}] packet to large to received, maximum is {}, got {}.",
                self.config.name,
                self.config.max_packet_size,
                payload.len()
            );
            return Err(RenetError::MaximumPacketSizeExceeded);
        }
        if let Some(payload) = self.process_payload(payload)? {
            return Ok(Some((payload, addrs)));
        }
        Ok(None)
    }

    pub fn process_payload(&mut self, payload: &[u8]) -> Result<Option<Vec<u8>>> {
        if payload[0] == PacketType::Packet as u8 {
            let header = PacketHeader::parse(payload)?;
            // Received packet to buffer
            let received_packet = ReceivedPacket::new(Instant::now(), payload.len());
            self.received_buffer
                .insert(header.sequence, received_packet);
            self.update_acket_packets(header.ack, header.ack_bits);
            let payload = &payload[header.size()..];
            debug!(
                "[{}] successfuly processed packet {}.",
                self.config.name, header.sequence
            );
            Ok(Some(payload.into()))
        } else if payload[0] == PacketType::Fragment as u8 {
            let fragment_header = FragmentHeader::parse(payload)?;

            if let Some(received_packet) = self.received_buffer.get_mut(fragment_header.sequence) {
                received_packet.size += payload.len();
            } else {
                let received_packet = ReceivedPacket::new(Instant::now(), payload.len());
                self.received_buffer
                    .insert(fragment_header.sequence, received_packet);
            }

            if let Some(ref packet_header) = fragment_header.packet_header {
                self.update_acket_packets(packet_header.ack, packet_header.ack_bits)
            }

            let payload = &payload[fragment_header.size()..];

            let payload = self.reassembly_buffer.handle_fragment(
                fragment_header,
                payload,
                &self.config.fragment_config,
            )?;
            if let Some(payload) = payload {
                return Ok(Some(payload));
            }
            Ok(None)
        } else if payload[0] == PacketType::Heartbeat as u8 {
            let heartbeat = HeartbeatHeader::parse(payload)?;
            self.update_acket_packets(heartbeat.ack, heartbeat.ack_bits);

            Ok(None)
        } else {
            Err(RenetError::InvalidHeaderType)
        }
    }

    pub fn update_sent_bandwidth(&mut self) {
        let sample_size = self.config.sent_packets_buffer_size / 4;
        let base_sequence = self.sent_buffer.sequence().wrapping_sub(sample_size as u16);

        let mut packets_dropped = 0;
        let mut bytes_sent = 0;
        let mut start_time = Instant::now();
        let mut end_time = Instant::now() - Duration::from_secs(100);
        for i in 0..sample_size {
            if let Some(sent_packet) = self.sent_buffer.get(base_sequence.wrapping_add(i as u16)) {
                if sent_packet.size == 0 {
                    // Only Default Packets have size 0
                    continue;
                }
                bytes_sent += sent_packet.size;
                if sent_packet.time < start_time {
                    start_time = sent_packet.time;
                }
                if sent_packet.time > end_time {
                    end_time = sent_packet.time;
                }
                if !sent_packet.ack {
                    packets_dropped += 1;
                }
            }
        }

        // Calculate packet loss
        let packet_loss = packets_dropped as f64 / sample_size as f64 * 100.0;
        if f64::abs(self.network_info.packet_loss - packet_loss) > 0.0001 {
            self.network_info.packet_loss += (packet_loss - self.network_info.packet_loss)
                * self.config.measure_smoothing_factor;
        } else {
            self.network_info.packet_loss = packet_loss;
        }

        // Calculate sent bandwidth
        if end_time <= start_time {
            return;
        }

        let sent_bandwidth_kbps =
            bytes_sent as f64 / (end_time - start_time).as_secs_f64() * 8.0 / 1000.0;
        if f64::abs(self.network_info.sent_bandwidth_kbps - sent_bandwidth_kbps) > 0.0001 {
            self.network_info.sent_bandwidth_kbps += (sent_bandwidth_kbps
                - self.network_info.sent_bandwidth_kbps)
                * self.config.measure_smoothing_factor;
        } else {
            self.network_info.sent_bandwidth_kbps = sent_bandwidth_kbps;
        }
    }

    pub fn update_received_bandwidth(&mut self) {
        let sample_size = self.config.received_packets_buffer_size / 4;
        let base_sequence = self
            .received_buffer
            .sequence()
            .wrapping_sub(sample_size as u16)
            .wrapping_add(1);

        let mut bytes_received = 0;
        let mut start_time = Instant::now();
        let mut end_time = Instant::now() - Duration::from_secs(100);
        for i in 0..sample_size {
            if let Some(received_packet) = self
                .received_buffer
                .get_mut(base_sequence.wrapping_add(i as u16))
            {
                bytes_received += received_packet.size;
                if received_packet.time < start_time {
                    start_time = received_packet.time;
                }
                if received_packet.time > end_time {
                    end_time = received_packet.time;
                }
            }
        }

        if end_time <= start_time {
            return;
        }

        let received_bandwidth_kbps =
            bytes_received as f64 / (end_time - start_time).as_secs_f64() * 8.0 / 1000.0;
        if f64::abs(self.network_info.received_bandwidth_kbps - received_bandwidth_kbps) > 0.0001 {
            self.network_info.received_bandwidth_kbps += (received_bandwidth_kbps
                - self.network_info.received_bandwidth_kbps)
                * self.config.measure_smoothing_factor;
        } else {
            self.network_info.received_bandwidth_kbps = received_bandwidth_kbps;
        }
    }

    fn update_acket_packets(&mut self, ack: u16, ack_bits: u32) {
        let mut ack_bits = ack_bits;
        let now = Instant::now();
        for i in 0..32 {
            if ack_bits & 1 != 0 {
                let ack_sequence = ack.wrapping_sub(i);
                if let Some(ref mut sent_packet) = self.sent_buffer.get_mut(ack_sequence) {
                    if !sent_packet.ack {
                        debug!("Acked packet {}.", ack_sequence);
                        self.acks.push(ack_sequence);
                        sent_packet.ack = true;
                        let rtt = (now - sent_packet.time).as_secs_f64();
                        if self.network_info.rtt == 0.0 && rtt > 0.0
                            || f64::abs(self.network_info.rtt - rtt) < 0.00001
                        {
                            self.network_info.rtt = rtt;
                        } else {
                            self.network_info.rtt += (rtt - self.network_info.rtt)
                                * self.config.measure_smoothing_factor;
                        }
                    }
                }
            }
            ack_bits >>= 1;
        }
    }

    pub fn reset_acks(&mut self) {
        self.acks.clear();
    }

    pub fn get_acks(&self) -> &[u16] {
        &self.acks
    }
}

fn build_normal_packet(payload: &[u8], sequence: u16, ack: u16, ack_bits: u32) -> Result<Vec<u8>> {
    let header = PacketHeader {
        sequence,
        ack,
        ack_bits,
    };

    let mut buffer = vec![0u8; header.size()];
    header.write(&mut buffer)?;
    buffer.extend_from_slice(&payload);

    Ok(buffer)
}
