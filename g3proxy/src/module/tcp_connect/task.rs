/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2023-2025 ByteDance and/or its affiliates.
 */

use std::net::SocketAddr;
use std::time::Duration;

use chrono::{DateTime, Utc};
use openssl::ssl::Ssl;

use g3_socket::BindAddr;
use g3_types::metrics::NodeName;
use g3_types::net::{EgressInfo, Host, OpensslClientConfig, UpstreamAddr};

use super::TcpConnectError;

pub(crate) struct TcpConnectTaskConf<'a> {
    pub(crate) upstream: &'a UpstreamAddr,
}

pub(crate) struct TlsConnectTaskConf<'a> {
    pub(crate) tcp: TcpConnectTaskConf<'a>,
    pub(crate) tls_config: &'a OpensslClientConfig,
    pub(crate) tls_name: &'a Host,
}

impl TlsConnectTaskConf<'_> {
    pub(crate) fn build_ssl(&self) -> Result<Ssl, TcpConnectError> {
        self.tls_config
            .build_ssl(self.tls_name, self.tcp.upstream.port())
            .map_err(TcpConnectError::InternalTlsClientError)
    }

    pub(crate) fn handshake_timeout(&self) -> Duration {
        self.tls_config.handshake_timeout
    }
}

/// This contains the final chained info about the client request
#[derive(Debug, Clone, Default)]
pub(crate) struct TcpConnectChainedNotes {
    pub(crate) target_addr: Option<SocketAddr>,
    pub(crate) outgoing_addr: Option<SocketAddr>,
}

impl TcpConnectChainedNotes {
    fn reset(&mut self) {
        self.target_addr = None;
        self.outgoing_addr = None;
    }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct TcpConnectTaskNotes {
    pub(crate) escaper: NodeName,
    pub(crate) bind: BindAddr,
    pub(crate) next: Option<SocketAddr>,
    pub(crate) tries: usize,
    pub(crate) local: Option<SocketAddr>,
    pub(crate) expire: Option<DateTime<Utc>>,
    pub(crate) egress: Option<EgressInfo>,
    pub(crate) chained: TcpConnectChainedNotes,
    pub(crate) duration: Duration,
}

impl TcpConnectTaskNotes {
    pub(crate) fn reset(&mut self) {
        self.escaper.clear();
        self.bind = BindAddr::None;
        self.next = None;
        self.tries = 0;
        self.local = None;
        self.expire = None;
        self.egress = None;
        self.chained.reset();
        self.duration = Duration::ZERO;
    }
}
