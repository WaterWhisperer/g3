/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2023-2025 ByteDance and/or its affiliates.
 */

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use arc_swap::ArcSwap;
use async_trait::async_trait;
use log::debug;
#[cfg(feature = "quic")]
use quinn::Connection;
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio_rustls::{TlsAcceptor, server::TlsStream};

use g3_daemon::listen::{AcceptQuicServer, AcceptTcpServer, ListenStats, ListenTcpRuntime};
use g3_daemon::server::{BaseServer, ClientConnectionInfo, ServerReloadCommand};
use g3_io_ext::haproxy::{ProxyProtocolV1Reader, ProxyProtocolV2Reader};
use g3_openssl::SslStream;
use g3_types::acl::{AclAction, AclNetworkRule};
use g3_types::metrics::NodeName;
use g3_types::net::{
    OpensslTicketKey, ProxyProtocolVersion, RollingTicketer, RustlsServerConnectionExt,
};

use crate::config::server::plain_tls_port::PlainTlsPortConfig;
use crate::config::server::{AnyServerConfig, ServerConfig};
use crate::serve::{
    ArcServer, ArcServerInternal, Server, ServerInternal, ServerQuitPolicy, ServerRegistry,
    WrapArcServer,
};

pub(crate) struct PlainTlsPort {
    config: PlainTlsPortConfig,
    listen_stats: Arc<ListenStats>,
    tls_rolling_ticketer: Option<Arc<RollingTicketer<OpensslTicketKey>>>,
    tls_acceptor: TlsAcceptor,
    tls_accept_timeout: Duration,
    ingress_net_filter: Option<AclNetworkRule>,
    reload_sender: broadcast::Sender<ServerReloadCommand>,

    next_server: ArcSwap<ArcServer>,
    quit_policy: Arc<ServerQuitPolicy>,
    reload_version: usize,
}

impl PlainTlsPort {
    fn new<F>(
        config: PlainTlsPortConfig,
        listen_stats: Arc<ListenStats>,
        tls_rolling_ticketer: Option<Arc<RollingTicketer<OpensslTicketKey>>>,
        reload_version: usize,
        mut fetch_server: F,
    ) -> anyhow::Result<Self>
    where
        F: FnMut(&NodeName) -> ArcServer,
    {
        let reload_sender = crate::serve::new_reload_notify_channel();

        let tls_server_config = if let Some(builder) = &config.server_tls_config {
            builder
                .build_with_ticketer(tls_rolling_ticketer.clone())
                .context("failed to build tls server config")?
        } else {
            return Err(anyhow!("no tls server config set"));
        };

        let ingress_net_filter = config
            .ingress_net_filter
            .as_ref()
            .map(|builder| builder.build());

        let next_server = Arc::new(fetch_server(&config.server));

        Ok(PlainTlsPort {
            config,
            listen_stats,
            tls_rolling_ticketer,
            tls_acceptor: TlsAcceptor::from(tls_server_config.driver),
            tls_accept_timeout: tls_server_config.accept_timeout,
            ingress_net_filter,
            reload_sender,
            next_server: ArcSwap::new(next_server),
            quit_policy: Arc::new(ServerQuitPolicy::default()),
            reload_version,
        })
    }

    pub(crate) fn prepare_initial(config: PlainTlsPortConfig) -> anyhow::Result<ArcServerInternal> {
        let listen_stats = Arc::new(ListenStats::new(config.name()));

        let tls_rolling_ticketer = if let Some(c) = &config.tls_ticketer {
            let ticketer = c
                .build_and_spawn_updater()
                .context("failed to create tls rolling ticketer")?;
            Some(ticketer)
        } else {
            None
        };

        let server = PlainTlsPort::new(
            config,
            listen_stats,
            tls_rolling_ticketer,
            1,
            crate::serve::get_or_insert_default,
        )?;
        Ok(Arc::new(server))
    }

    fn prepare_reload(
        &self,
        config: AnyServerConfig,
        registry: &mut ServerRegistry,
    ) -> anyhow::Result<PlainTlsPort> {
        if let AnyServerConfig::PlainTlsPort(config) = config {
            let listen_stats = Arc::clone(&self.listen_stats);

            let tls_rolling_ticketer = if self.config.tls_ticketer.eq(&config.tls_ticketer) {
                self.tls_rolling_ticketer.clone()
            } else if let Some(c) = &config.tls_ticketer {
                let ticketer = c
                    .build_and_spawn_updater()
                    .context("failed to create tls rolling ticketer")?;
                Some(ticketer)
            } else {
                None
            };

            PlainTlsPort::new(
                config,
                listen_stats,
                tls_rolling_ticketer,
                self.reload_version + 1,
                |name| registry.get_or_insert_default(name),
            )
        } else {
            Err(anyhow!(
                "config type mismatch: expect {}, actual {}",
                self.config.r#type(),
                config.r#type()
            ))
        }
    }

    fn drop_early(&self, client_addr: SocketAddr) -> bool {
        if let Some(ingress_net_filter) = &self.ingress_net_filter {
            let (_, action) = ingress_net_filter.check(client_addr.ip());
            match action {
                AclAction::Permit | AclAction::PermitAndLog => {}
                AclAction::Forbid | AclAction::ForbidAndLog => {
                    self.listen_stats.add_dropped();
                    return true;
                }
            }
        }

        // TODO add cps limit

        false
    }

    async fn run_task(&self, mut stream: TcpStream, mut cc_info: ClientConnectionInfo) {
        match self.config.proxy_protocol {
            Some(ProxyProtocolVersion::V1) => {
                let mut parser =
                    ProxyProtocolV1Reader::new(self.config.proxy_protocol_read_timeout);
                match parser.read_proxy_protocol_v1_for_tcp(&mut stream).await {
                    Ok(Some(a)) => cc_info.set_proxy_addr(a),
                    Ok(None) => {}
                    Err(e) => {
                        self.listen_stats.add_by_proxy_protocol_error(e);
                        return;
                    }
                }
            }
            Some(ProxyProtocolVersion::V2) => {
                let mut parser =
                    ProxyProtocolV2Reader::new(self.config.proxy_protocol_read_timeout);
                match parser.read_proxy_protocol_v2_for_tcp(&mut stream).await {
                    Ok(Some(a)) => cc_info.set_proxy_addr(a),
                    Ok(None) => {}
                    Err(e) => {
                        self.listen_stats.add_by_proxy_protocol_error(e);
                        return;
                    }
                }
            }
            None => {}
        }

        match tokio::time::timeout(self.tls_accept_timeout, self.tls_acceptor.accept(stream)).await
        {
            Ok(Ok(tls_stream)) => {
                if tls_stream.get_ref().1.session_reused() {
                    // Quick ACK is needed with session resumption
                    cc_info.tcp_sock_try_quick_ack();
                }
                let next_server = self.next_server.load().as_ref().clone();
                next_server.run_rustls_task(tls_stream, cc_info).await
            }
            Ok(Err(e)) => {
                self.listen_stats.add_failed();
                debug!(
                    "{} - {} tls error: {e:?}",
                    cc_info.sock_local_addr(),
                    cc_info.sock_peer_addr()
                );
                // TODO record tls failure and add some sec policy
            }
            Err(_) => {
                self.listen_stats.add_timeout();
                debug!(
                    "{} - {} tls timeout",
                    cc_info.sock_local_addr(),
                    cc_info.sock_peer_addr()
                );
                // TODO record tls failure and add some sec policy
            }
        }
    }
}

impl ServerInternal for PlainTlsPort {
    fn _clone_config(&self) -> AnyServerConfig {
        AnyServerConfig::PlainTlsPort(self.config.clone())
    }

    fn _depend_on_server(&self, name: &NodeName) -> bool {
        self.config.server.eq(name)
    }

    fn _reload_config_notify_runtime(&self) {
        let cmd = ServerReloadCommand::ReloadVersion(self.reload_version);
        let _ = self.reload_sender.send(cmd);
    }

    fn _update_next_servers_in_place(&self) {
        let next_server = crate::serve::get_or_insert_default(&self.config.server);
        self.next_server.store(Arc::new(next_server));
    }

    fn _update_escaper_in_place(&self) {}
    fn _update_user_group_in_place(&self) {}
    fn _update_audit_handle_in_place(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn _reload_with_old_notifier(
        &self,
        config: AnyServerConfig,
        registry: &mut ServerRegistry,
    ) -> anyhow::Result<ArcServerInternal> {
        let mut server = self.prepare_reload(config, registry)?;
        server.reload_sender = self.reload_sender.clone();
        Ok(Arc::new(server))
    }

    fn _reload_with_new_notifier(
        &self,
        config: AnyServerConfig,
        registry: &mut ServerRegistry,
    ) -> anyhow::Result<ArcServerInternal> {
        let server = self.prepare_reload(config, registry)?;
        Ok(Arc::new(server))
    }

    fn _start_runtime(&self, server: ArcServer) -> anyhow::Result<()> {
        let listen_stats = server.get_listen_stats();
        let runtime = ListenTcpRuntime::new(WrapArcServer(server), listen_stats);
        runtime.run_all_instances(
            &self.config.listen,
            self.config.listen_in_worker,
            &self.reload_sender,
        )
    }

    fn _abort_runtime(&self) {
        let _ = self.reload_sender.send(ServerReloadCommand::QuitRuntime);
    }
}

impl BaseServer for PlainTlsPort {
    #[inline]
    fn name(&self) -> &NodeName {
        self.config.name()
    }

    #[inline]
    fn r#type(&self) -> &'static str {
        self.config.r#type()
    }

    #[inline]
    fn version(&self) -> usize {
        self.reload_version
    }
}

#[async_trait]
impl AcceptTcpServer for PlainTlsPort {
    async fn run_tcp_task(&self, stream: TcpStream, cc_info: ClientConnectionInfo) {
        let client_addr = cc_info.client_addr();
        if self.drop_early(client_addr) {
            return;
        }

        self.run_task(stream, cc_info).await
    }
}

#[async_trait]
impl AcceptQuicServer for PlainTlsPort {
    #[cfg(feature = "quic")]
    async fn run_quic_task(&self, _connection: Connection, _cc_info: ClientConnectionInfo) {}
}

#[async_trait]
impl Server for PlainTlsPort {
    fn escaper(&self) -> &NodeName {
        Default::default()
    }

    fn user_group(&self) -> &NodeName {
        Default::default()
    }

    fn auditor(&self) -> &NodeName {
        Default::default()
    }

    fn get_listen_stats(&self) -> Arc<ListenStats> {
        Arc::clone(&self.listen_stats)
    }

    fn alive_count(&self) -> i32 {
        0
    }

    #[inline]
    fn quit_policy(&self) -> &Arc<ServerQuitPolicy> {
        &self.quit_policy
    }

    async fn run_rustls_task(&self, _stream: TlsStream<TcpStream>, _cc_info: ClientConnectionInfo) {
    }

    async fn run_openssl_task(
        &self,
        _stream: SslStream<TcpStream>,
        _cc_info: ClientConnectionInfo,
    ) {
    }
}
