//! Configuration for QUIC connections using tquic.

use std::time::Duration;
use tquic::CongestionControlAlgorithm;

/// Configuration for QUIC endpoints.
#[derive(Clone)]
pub struct Config {
    /// Maximum number of concurrent connections.
    pub max_connections: u32,

    /// Enable multipath QUIC.
    pub enable_multipath: bool,

    /// Congestion control algorithm.
    pub congestion_control: CongestionControlAlgorithm,

    /// Keep-alive interval.
    pub keep_alive_interval: Duration,

    /// Maximum idle timeout.
    pub idle_timeout: Duration,

    /// Initial RTT estimate in milliseconds.
    pub initial_rtt_ms: u64,

    /// TLS certificate path (for server).
    pub cert_path: Option<String>,

    /// TLS private key path (for server).
    pub key_path: Option<String>,

    /// TLS root CA path (for client certificate verification).
    pub ca_path: Option<String>,

    /// ALPN protocols.
    pub alpn: Vec<Vec<u8>>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_connections: 256,
            enable_multipath: true,
            congestion_control: CongestionControlAlgorithm::Bbr,
            keep_alive_interval: Duration::from_millis(400),
            idle_timeout: Duration::from_secs(30),
            initial_rtt_ms: 100,
            cert_path: None,
            key_path: None,
            ca_path: None,
            alpn: vec![b"picoquic_sample".to_vec()],
        }
    }
}

impl Config {
    /// Create a new config with defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the congestion control algorithm.
    pub fn with_congestion_control(mut self, algo: CongestionControlAlgorithm) -> Self {
        self.congestion_control = algo;
        self
    }

    /// Enable or disable multipath.
    pub fn with_multipath(mut self, enable: bool) -> Self {
        self.enable_multipath = enable;
        self
    }

    /// Set the keep-alive interval.
    pub fn with_keep_alive(mut self, interval: Duration) -> Self {
        self.keep_alive_interval = interval;
        self
    }

    /// Set the TLS certificate and key paths (for server).
    pub fn with_tls(mut self, cert: &str, key: &str) -> Self {
        self.cert_path = Some(cert.to_string());
        self.key_path = Some(key.to_string());
        self
    }

    /// Set the root CA path (for client verification).
    pub fn with_ca(mut self, ca: &str) -> Self {
        self.ca_path = Some(ca.to_string());
        self
    }

    /// Convert to tquic Config for client.
    pub fn to_tquic_client_config(&self) -> Result<tquic::Config, crate::Error> {
        let mut config = tquic::Config::new().map_err(|e| crate::Error::Config(e.to_string()))?;

        // Enable multipath
        config.enable_multipath(self.enable_multipath);

        // Set congestion control
        config.set_congestion_control_algorithm(self.congestion_control);

        // Set timeouts
        config.set_max_idle_timeout(self.idle_timeout.as_millis() as u64);

        // Set initial RTT
        config.set_initial_rtt(self.initial_rtt_ms);

        Ok(config)
    }

    /// Convert to tquic Config for server.
    pub fn to_tquic_server_config(&self) -> Result<tquic::Config, crate::Error> {
        let mut config = self.to_tquic_client_config()?;

        // Server-specific setup will be done when creating the endpoint
        Ok(config)
    }
}
