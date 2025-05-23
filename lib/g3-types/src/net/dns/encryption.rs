/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2023-2025 ByteDance and/or its affiliates.
 */

use std::str::FromStr;

use anyhow::anyhow;
#[cfg(feature = "rustls")]
use rustls_pki_types::ServerName;

#[cfg(feature = "rustls")]
use crate::net::{RustlsClientConfig, RustlsClientConfigBuilder};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DnsEncryptionProtocol {
    Tls,
    Https,
    #[cfg(feature = "quic")]
    H3,
    #[cfg(feature = "quic")]
    Quic,
}

impl FromStr for DnsEncryptionProtocol {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().replace('-', "_").as_str() {
            "tls" | "dns_over_tls" | "dnsovertls" | "dot" => Ok(DnsEncryptionProtocol::Tls),
            "https" | "h2" | "dns_over_https" | "dnsoverhttps" | "doh" => {
                Ok(DnsEncryptionProtocol::Https)
            }
            #[cfg(feature = "quic")]
            "h3" | "http/3" | "dns_over_http/3" | "dnsoverhttp/3" | "doh3" => {
                Ok(DnsEncryptionProtocol::H3)
            }
            #[cfg(feature = "quic")]
            "quic" | "dns_over_quic" | "dnsoverquic" | "doq" => Ok(DnsEncryptionProtocol::Quic),
            _ => Err(anyhow!("unknown protocol {}", s)),
        }
    }
}

impl DnsEncryptionProtocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            DnsEncryptionProtocol::Tls => "DnsOverTls",
            DnsEncryptionProtocol::Https => "DnsOverHttps",
            #[cfg(feature = "quic")]
            DnsEncryptionProtocol::H3 => "DnsOverHttp/3",
            #[cfg(feature = "quic")]
            DnsEncryptionProtocol::Quic => "DnsOverQuic",
        }
    }

    pub fn default_port(&self) -> u16 {
        match self {
            DnsEncryptionProtocol::Tls => 853,
            DnsEncryptionProtocol::Https => 443,
            #[cfg(feature = "quic")]
            DnsEncryptionProtocol::H3 => 443,
            #[cfg(feature = "quic")]
            DnsEncryptionProtocol::Quic => 853,
        }
    }
}

#[derive(Clone)]
#[cfg(feature = "rustls")]
pub struct DnsEncryptionConfig {
    protocol: DnsEncryptionProtocol,
    tls_name: ServerName<'static>,
    tls_client: RustlsClientConfig,
}

#[cfg(feature = "rustls")]
impl DnsEncryptionConfig {
    #[inline]
    pub fn protocol(&self) -> DnsEncryptionProtocol {
        self.protocol
    }

    #[inline]
    pub fn tls_name(&self) -> &ServerName<'static> {
        &self.tls_name
    }

    #[inline]
    pub fn tls_client(&self) -> &RustlsClientConfig {
        &self.tls_client
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(feature = "rustls")]
pub struct DnsEncryptionConfigBuilder {
    protocol: DnsEncryptionProtocol,
    tls_name: ServerName<'static>,
    tls_config: RustlsClientConfigBuilder,
}

#[cfg(feature = "rustls")]
impl DnsEncryptionConfigBuilder {
    pub fn new(tls_name: ServerName<'static>) -> Self {
        DnsEncryptionConfigBuilder {
            protocol: DnsEncryptionProtocol::Tls,
            tls_name,
            tls_config: RustlsClientConfigBuilder::default(),
        }
    }

    pub fn set_protocol(&mut self, protocol: DnsEncryptionProtocol) {
        self.protocol = protocol;
    }

    #[inline]
    pub fn protocol(&self) -> DnsEncryptionProtocol {
        self.protocol
    }

    pub fn set_tls_name(&mut self, name: ServerName<'static>) {
        self.tls_name = name;
    }

    #[inline]
    pub fn tls_name(&self) -> &ServerName<'static> {
        &self.tls_name
    }

    pub fn set_tls_client_config(&mut self, config_builder: RustlsClientConfigBuilder) {
        self.tls_config = config_builder;
    }

    pub fn summary(&self) -> String {
        match &self.tls_name {
            ServerName::DnsName(n) => format!("{}({})", self.protocol.as_str(), n.as_ref()),
            ServerName::IpAddress(ip) => {
                format!(
                    "{}({})",
                    self.protocol.as_str(),
                    std::net::IpAddr::from(*ip)
                )
            }
            _ => format!("{}(other)", self.protocol.as_str()), // FIXME support other server name variants
        }
    }

    pub fn build_tls_client_config(&self) -> anyhow::Result<RustlsClientConfig> {
        self.tls_config.build()
    }

    pub fn build(&self) -> anyhow::Result<DnsEncryptionConfig> {
        let tls_client = self.tls_config.build()?;
        Ok(DnsEncryptionConfig {
            protocol: self.protocol,
            tls_name: self.tls_name.clone(),
            tls_client,
        })
    }
}
