/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2025 ByteDance and/or its affiliates.
 */

use std::sync::Arc;

use anyhow::anyhow;
use arc_swap::ArcSwap;
use chrono::Utc;
use log::debug;
use tokio::net::unix::SocketAddr;
use tokio::sync::broadcast;

use g3_daemon::listen::{ReceiveUdpServer, ReceiveUnixDatagramRuntime, ReceiveUnixDatagramServer};
use g3_daemon::server::{BaseServer, ServerReloadCommand};
use g3_types::metrics::NodeName;

use super::StatsdRecordVisitor;
use crate::collect::ArcCollector;
use crate::config::importer::statsd::StatsdUnixImporterConfig;
use crate::config::importer::{AnyImporterConfig, ImporterConfig};
use crate::import::{
    ArcImporter, ArcImporterInternal, Importer, ImporterInternal, ImporterRegistry, WrapArcImporter,
};

pub(crate) struct StatsdUnixImporter {
    config: StatsdUnixImporterConfig,
    reload_sender: broadcast::Sender<ServerReloadCommand>,

    collector: ArcSwap<ArcCollector>,
    reload_version: usize,
}

impl StatsdUnixImporter {
    fn new(config: StatsdUnixImporterConfig, reload_version: usize) -> Self {
        let reload_sender = crate::import::new_reload_notify_channel();

        let collector = Arc::new(crate::collect::get_or_insert_default(config.collector()));

        StatsdUnixImporter {
            config,
            reload_sender,
            collector: ArcSwap::new(collector),
            reload_version,
        }
    }

    pub(crate) fn prepare_initial(
        config: StatsdUnixImporterConfig,
    ) -> anyhow::Result<ArcImporterInternal> {
        let server = StatsdUnixImporter::new(config, 1);
        Ok(Arc::new(server))
    }

    fn prepare_reload(&self, config: AnyImporterConfig) -> anyhow::Result<StatsdUnixImporter> {
        if let AnyImporterConfig::StatsDUnix(config) = config {
            Ok(StatsdUnixImporter::new(config, self.reload_version + 1))
        } else {
            Err(anyhow!(
                "config type mismatch: expect {}, actual {}",
                self.config.importer_type(),
                config.importer_type()
            ))
        }
    }
}

impl ImporterInternal for StatsdUnixImporter {
    fn _clone_config(&self) -> AnyImporterConfig {
        AnyImporterConfig::StatsDUnix(self.config.clone())
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
        let runtime = ReceiveUnixDatagramRuntime::new(
            WrapArcImporter(importer.clone()),
            self.config.listen.clone(),
        );
        runtime.spawn(&self.reload_sender)
    }

    fn _abort_runtime(&self) {
        let _ = self.reload_sender.send(ServerReloadCommand::QuitRuntime);
    }
}

impl BaseServer for StatsdUnixImporter {
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

impl ReceiveUdpServer for StatsdUnixImporter {
    fn receive_udp_packet(
        &self,
        _packet: &[u8],
        _client_addr: std::net::SocketAddr,
        _server_addr: std::net::SocketAddr,
        _worker_id: Option<usize>,
    ) {
    }
}

impl ReceiveUnixDatagramServer for StatsdUnixImporter {
    fn receive_unix_packet(&self, packet: &[u8], client_addr: SocketAddr) {
        let time = Utc::now();
        let iter = StatsdRecordVisitor::new(packet);
        for r in iter {
            match r {
                Ok(r) => self.collector.load().add_metric(time, r, None),
                Err(e) => {
                    debug!("invalid StatsD record from {client_addr:?}: {e}");
                }
            }
        }
    }
}

impl Importer for StatsdUnixImporter {
    fn collector(&self) -> &NodeName {
        self.config.collector()
    }
}
