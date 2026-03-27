use anyhow::{Context, Result};
use prost::Message;
use protocol::resc_control;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// TCP control channel client. Connects to the Mac host.
/// Framing: u32_le length + protobuf Envelope bytes.
pub struct ControlChannel {
    stream: TcpStream,
    session_id: u64,
}

impl ControlChannel {
    /// Connect to the host's control port.
    pub async fn connect(host: &str, port: u16) -> Result<Self> {
        let addr = format!("{}:{}", host, port);
        let stream = TcpStream::connect(&addr).await
            .context(format!("Failed to connect to {}", addr))?;
        log::info!("Control channel connected to {}", addr);

        Ok(Self { stream, session_id: 0 })
    }

    /// Send a protobuf Envelope with u32_le length prefix.
    pub async fn send(&mut self, envelope: &resc_control::Envelope) -> Result<()> {
        let payload = envelope.encode_to_vec();
        let len = (payload.len() as u32).to_le_bytes();
        self.stream.write_all(&len).await?;
        self.stream.write_all(&payload).await?;
        Ok(())
    }

    /// Receive a protobuf Envelope (blocking until complete message arrives).
    pub async fn recv(&mut self) -> Result<resc_control::Envelope> {
        // Read u32_le length
        let mut len_buf = [0u8; 4];
        self.stream.read_exact(&mut len_buf).await?;
        let len = u32::from_le_bytes(len_buf) as usize;

        if len == 0 || len > 1_000_000 {
            anyhow::bail!("Invalid message length: {}", len);
        }

        // Read payload
        let mut payload = vec![0u8; len];
        self.stream.read_exact(&mut payload).await?;

        let envelope = resc_control::Envelope::decode(&payload[..])
            .context("Failed to decode Envelope")?;

        // Track session_id
        if envelope.session_id != 0 {
            self.session_id = envelope.session_id;
        }

        Ok(envelope)
    }

    /// Perform mode negotiation handshake.
    /// Returns the ModeConfirm from the host.
    pub async fn negotiate_mode(
        &mut self,
        preferred_width: u32,
        preferred_height: u32,
        preferred_refresh_millihz: u32,
    ) -> Result<resc_control::ModeConfirm> {
        // Send ModeRequest
        let mode_request = resc_control::ModeRequest {
            protocol_version: protocol::constants::PROTOCOL_VERSION as u32,
            preferred_modes: vec![resc_control::DisplayMode {
                width: preferred_width,
                height: preferred_height,
                refresh_rate_millihz: preferred_refresh_millihz,
            }],
            rotation: 0,
            supported_codecs: vec![resc_control::Codec::H264 as i32],
        };

        let envelope = resc_control::Envelope {
            session_id: 0,
            protocol_version: protocol::constants::PROTOCOL_VERSION as u32,
            payload: Some(resc_control::envelope::Payload::ModeRequest(mode_request)),
        };

        self.send(&envelope).await?;
        log::info!("Sent ModeRequest: {}x{}@{}mHz", preferred_width, preferred_height, preferred_refresh_millihz);

        // Wait for ModeConfirm or ModeReject
        let response = self.recv().await?;
        match response.payload {
            Some(resc_control::envelope::Payload::ModeConfirm(confirm)) => {
                self.session_id = confirm.session_id;
                log::info!(
                    "ModeConfirm: {}x{}, stream_id={}, config_id={}, codec={:?}",
                    confirm.actual_width, confirm.actual_height,
                    confirm.stream_id, confirm.config_id, confirm.codec
                );
                Ok(confirm)
            }
            Some(resc_control::envelope::Payload::ModeReject(reject)) => {
                anyhow::bail!("Mode rejected: {:?} - {}", reject.reason(), reject.message);
            }
            other => {
                anyhow::bail!("Unexpected response: {:?}", other);
            }
        }
    }

    /// Wait for StartStreaming, then send StreamingReady.
    pub async fn wait_for_start_streaming(&mut self, stream_id: u32, config_id: u32) -> Result<()> {
        let msg = self.recv().await?;
        match msg.payload {
            Some(resc_control::envelope::Payload::StartStreaming(start)) => {
                if start.stream_id != stream_id || start.config_id != config_id {
                    anyhow::bail!("StartStreaming ID mismatch");
                }
            }
            other => anyhow::bail!("Expected StartStreaming, got: {:?}", other),
        }

        // Send StreamingReady
        let ready = resc_control::Envelope {
            session_id: self.session_id,
            protocol_version: protocol::constants::PROTOCOL_VERSION as u32,
            payload: Some(resc_control::envelope::Payload::StreamingReady(
                resc_control::StreamingReady { stream_id, config_id },
            )),
        };
        self.send(&ready).await?;
        log::info!("StreamingReady sent");
        Ok(())
    }

    /// Send Stats to host.
    pub async fn send_stats(
        &mut self,
        packet_loss_rate: f32,
        frame_drop_rate: f32,
        decode_ms_p95: u32,
    ) -> Result<()> {
        let envelope = resc_control::Envelope {
            session_id: self.session_id,
            protocol_version: protocol::constants::PROTOCOL_VERSION as u32,
            payload: Some(resc_control::envelope::Payload::Stats(resc_control::Stats {
                packet_loss_rate,
                frame_drop_rate,
                decode_ms_p95,
                misrouted_packets: 0,
                unsupported_version_packets: 0,
            })),
        };
        self.send(&envelope).await
    }

    /// Send RequestIDR to host (asks for keyframe).
    pub async fn send_request_idr(&mut self, stream_id: u32, config_id: u32, reason: i32) -> Result<()> {
        let envelope = resc_control::Envelope {
            session_id: self.session_id,
            protocol_version: protocol::constants::PROTOCOL_VERSION as u32,
            payload: Some(resc_control::envelope::Payload::RequestIdr(resc_control::RequestIdr {
                stream_id,
                config_id,
                reason,
            })),
        };
        self.send(&envelope).await
    }

    pub fn session_id(&self) -> u64 { self.session_id }
}
