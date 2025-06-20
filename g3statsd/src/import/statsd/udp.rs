/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2025 ByteDance and/or its affiliates.
 */

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::anyhow;
use arc_swap::ArcSwap;
use chrono::Utc;
use log::debug;
#[cfg(unix)]
use tokio::net::unix::SocketAddr as UnixSocketAddr;
use tokio::sync::broadcast;

#[cfg(unix)]
use g3_daemon::listen::ReceiveUnixDatagramServer;
use g3_daemon::listen::{ReceiveUdpRuntime, ReceiveUdpServer};
use g3_daemon::server::{BaseServer, ServerReloadCommand};
use g3_types::acl::{AclAction, AclNetworkRule};
use g3_types::metrics::NodeName;

use super::StatsdRecordVisitor;
use crate::collect::ArcCollector;
use crate::config::importer::statsd::StatsdUdpImporterConfig;
use crate::config::importer::{AnyImporterConfig, ImporterConfig};
use crate::import::{
    ArcImporter, ArcImporterInternal, Importer, ImporterInternal, ImporterRegistry, WrapArcImporter,
};

pub(crate) struct StatsdUdpImporter {
    config: StatsdUdpImporterConfig,
    ingress_net_filter: Option<AclNetworkRule>,
    reload_sender: broadcast::Sender<ServerReloadCommand>,

    collector: ArcSwap<ArcCollector>,
    reload_version: usize,
}

impl StatsdUdpImporter {
    fn new(config: StatsdUdpImporterConfig, reload_version: usize) -> Self {
        let reload_sender = crate::import::new_reload_notify_channel();

        let ingress_net_filter = config
            .ingress_net_filter
            .as_ref()
            .map(|builder| builder.build());

        let collector = Arc::new(crate::collect::get_or_insert_default(config.collector()));

        StatsdUdpImporter {
            config,
            ingress_net_filter,
            reload_sender,
            collector: ArcSwap::new(collector),
            reload_version,
        }
    }

    pub(crate) fn prepare_initial(
        config: StatsdUdpImporterConfig,
    ) -> anyhow::Result<ArcImporterInternal> {
        let server = StatsdUdpImporter::new(config, 1);
        Ok(Arc::new(server))
    }

    fn prepare_reload(&self, config: AnyImporterConfig) -> anyhow::Result<StatsdUdpImporter> {
        if let AnyImporterConfig::StatsDUdp(config) = config {
            Ok(StatsdUdpImporter::new(config, self.reload_version + 1))
        } else {
            Err(anyhow!(
                "config type mismatch: expect {}, actual {}",
                self.config.importer_type(),
                config.importer_type()
            ))
        }
    }

    fn drop_early(&self, client_addr: SocketAddr) -> bool {
        if let Some(ingress_net_filter) = &self.ingress_net_filter {
            let (_, action) = ingress_net_filter.check(client_addr.ip());
            match action {
                AclAction::Permit | AclAction::PermitAndLog => {}
                AclAction::Forbid | AclAction::ForbidAndLog => {
                    return true;
                }
            }
        }

        // TODO add cps limit

        false
    }
}

impl ImporterInternal for StatsdUdpImporter {
    fn _clone_config(&self) -> AnyImporterConfig {
        AnyImporterConfig::StatsDUdp(self.config.clone())
    }

    fn _reload_config_notify_runtime(&self) {
        let cmd = ServerReloadCommand::ReloadVersion(self.reload_version);
        let _ = self.reload_sender.send(cmd);
    }

    fn _update_collector_in_place(&self) {
        let collector = crate::collect::get_or_insert_default(self.config.collector());
        self.collector.store(Arc::new(collector));
    }

    fn _reload_with_old_notifier(
        &self,
        config: AnyImporterConfig,
        _registry: &mut ImporterRegistry,
    ) -> anyhow::Result<ArcImporterInternal> {
        let mut server = self.prepare_reload(config)?;
        server.reload_sender = self.reload_sender.clone();
        Ok(Arc::new(server))
    }

    fn _reload_with_new_notifier(
        &self,
        config: AnyImporterConfig,
        _registry: &mut ImporterRegistry,
    ) -> anyhow::Result<ArcImporterInternal> {
        let server = self.prepare_reload(config)?;
        Ok(Arc::new(server))
    }

    fn _start_runtime(&self, importer: ArcImporter) -> anyhow::Result<()> {
        let runtime = ReceiveUdpRuntime::new(
            WrapArcImporter(importer.clone()),
            self.config.listen.clone(),
        );
        runtime.run_all_instances(self.config.listen_in_worker, &self.reload_sender)
    }

    fn _abort_runtime(&self) {
        let _ = self.reload_sender.send(ServerReloadCommand::QuitRuntime);
    }
}

impl BaseServer for StatsdUdpImporter {
    #[inline]
    fn name(&self) -> &NodeName {
        self.config.name()
    }

    #[inline]
    fn r#type(&self) -> &'static str {
        self.config.importer_type()
    }

    #[inline]
    fn version(&self) -> usize {
        self.reload_version
    }
}

impl ReceiveUdpServer for StatsdUdpImporter {
    fn receive_udp_packet(
        &self,
        packet: &[u8],
        client_addr: SocketAddr,
        _server_addr: SocketAddr,
        worker_id: Option<usize>,
    ) {
        if self.drop_early(client_addr) {
            return;
        }

        let time = Utc::now();
        let iter = StatsdRecordVisitor::new(packet);
        for r in iter {
            match r {
                Ok(r) => self.collector.load().add_metric(time, r, worker_id),
                Err(e) => {
                    debug!("invalid StatsD record from {client_addr}: {e}");
                }
            }
        }
    }
}

#[cfg(unix)]
impl ReceiveUnixDatagramServer for StatsdUdpImporter {
    fn receive_unix_packet(&self, _packet: &[u8], _peer_addr: UnixSocketAddr) {}
}

impl Importer for StatsdUdpImporter {
    fn collector(&self) -> &NodeName {
        self.config.collector()
    }
}
