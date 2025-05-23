/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2024-2025 ByteDance and/or its affiliates.
 */

use std::sync::Arc;

use log::{debug, warn};
use tokio::sync::{broadcast, mpsc};

pub mod config;

mod build;

pub mod opts;
use opts::ProcArgs;

mod stat;

mod frontend;
use frontend::{Frontend, FrontendStats};

pub async fn run(proc_args: &ProcArgs) -> anyhow::Result<()> {
    let frontend_stats = Arc::new(FrontendStats::default());
    let (quit_sender, _) = broadcast::channel(1);
    let (wait_sender, mut wait_receiver) =
        mpsc::channel(g3_daemon::runtime::worker::worker_count().max(1));

    if let Some(stats_config) = g3_daemon::stat::config::get_global_stat_config() {
        stat::spawn_working_thread(stats_config, frontend_stats.clone())?;
    }

    let workers = g3_daemon::runtime::worker::foreach(|h| {
        let frontend = Frontend::new(proc_args.listen_config(), frontend_stats.clone())?;
        let quit_receiver = quit_sender.subscribe();
        let wait_sender = wait_sender.clone();
        let id = h.id;
        h.handle.spawn(async move {
            let _ = frontend.run(quit_receiver).await;
            let _ = wait_sender.try_send(Some(id));
        });
        Ok::<(), anyhow::Error>(())
    })?;
    if workers < 1 {
        let frontend = Frontend::new(proc_args.listen_config(), frontend_stats.clone())?;
        let quit_receiver = quit_sender.subscribe();
        let wait_sender = wait_sender.clone();
        tokio::spawn(async move {
            let _ = frontend.run(quit_receiver).await;
            let _ = wait_sender.try_send(None);
        });
    }

    if let Err(e) = tokio::signal::ctrl_c().await {
        warn!("failed to recv Ctrl-C signal: {e}");
    }
    debug!("received Ctrl-C signal, start shutdown now");
    drop(quit_sender);

    drop(wait_sender);
    while let Some(id) = wait_receiver.recv().await {
        if let Some(id) = id {
            debug!("all requests in worker {id} served");
        }
    }
    debug!("all requests served, quit now");
    Ok(())
}
