/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2023-2025 ByteDance and/or its affiliates.
 */

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use log::{info, warn};
use tokio::net::TcpStream;
use tokio::runtime::Handle;
use tokio::sync::broadcast;

use g3_compat::CpuAffinity;
use g3_io_ext::LimitedTcpListener;
use g3_socket::RawSocket;
use g3_std_ext::net::SocketAddrExt;
use g3_types::net::TcpListenConfig;

use crate::listen::{ListenAliveGuard, ListenStats};
use crate::server::{BaseServer, ClientConnectionInfo, ReloadServer, ServerReloadCommand};

#[async_trait]
pub trait AcceptTcpServer: BaseServer {
    async fn run_tcp_task(&self, stream: TcpStream, cc_info: ClientConnectionInfo);
}

#[derive(Clone)]
pub struct ListenTcpRuntime<S> {
    server: S,
    listen_stats: Arc<ListenStats>,
}

impl<S> ListenTcpRuntime<S>
where
    S: AcceptTcpServer + ReloadServer + Clone + Send + Sync + 'static,
{
    pub fn new(server: S, listen_stats: Arc<ListenStats>) -> Self {
        ListenTcpRuntime {
            server,
            listen_stats,
        }
    }

    fn create_instance(&self) -> ListenTcpRuntimeInstance<S> {
        let server_type = self.server.r#type();
        let server_version = self.server.version();
        ListenTcpRuntimeInstance {
            server: self.server.clone(),
            server_type,
            server_version,
            worker_id: None,
            #[cfg(target_os = "linux")]
            follow_incoming_cpu: false,
            listen_stats: self.listen_stats.clone(),
            instance_id: 0,
            _alive_guard: None,
        }
    }

    pub fn run_all_instances(
        &self,
        listen_config: &TcpListenConfig,
        listen_in_worker: bool,
        server_reload_sender: &broadcast::Sender<ServerReloadCommand>,
    ) -> anyhow::Result<()> {
        let mut instance_count = listen_config.instance();
        if listen_in_worker {
            let worker_count = crate::runtime::worker::worker_count();
            if worker_count > 0 {
                instance_count = worker_count;
            }
        }

        for i in 0..instance_count {
            let mut runtime = self.create_instance();
            runtime.instance_id = i;

            let listener = g3_socket::tcp::new_std_listener(listen_config)?;
            runtime.into_running(
                listener,
                listen_in_worker,
                listen_config.follow_cpu_affinity(),
                server_reload_sender.subscribe(),
            );
        }
        Ok(())
    }
}

pub struct ListenTcpRuntimeInstance<S> {
    server: S,
    server_type: &'static str,
    server_version: usize,
    worker_id: Option<usize>,
    #[cfg(target_os = "linux")]
    follow_incoming_cpu: bool,
    listen_stats: Arc<ListenStats>,
    instance_id: usize,
    _alive_guard: Option<ListenAliveGuard>,
}

impl<S> ListenTcpRuntimeInstance<S>
where
    S: AcceptTcpServer + ReloadServer + Clone + Send + Sync + 'static,
{
    fn pre_start(&mut self) {
        info!(
            "started {} SRT[{}_v{}#{}]",
            self.server_type,
            self.server.name(),
            self.server_version,
            self.instance_id,
        );
        self._alive_guard = Some(self.listen_stats.add_running_runtime());
    }

    fn pre_stop(&self) {
        info!(
            "stopping {} SRT[{}_v{}#{}]",
            self.server_type,
            self.server.name(),
            self.server_version,
            self.instance_id,
        );
    }

    fn post_stop(&self) {
        info!(
            "stopped {} SRT[{}_v{}#{}]",
            self.server_type,
            self.server.name(),
            self.server_version,
            self.instance_id,
        );
    }

    async fn run(
        mut self,
        mut listener: LimitedTcpListener,
        mut server_reload_channel: broadcast::Receiver<ServerReloadCommand>,
    ) {
        use broadcast::error::RecvError;

        loop {
            tokio::select! {
                biased;

                ev = server_reload_channel.recv() => {
                   match ev {
                        Ok(ServerReloadCommand::ReloadVersion(version)) => {
                            info!("SRT[{}_v{}#{}] received reload request from v{version}",
                                self.server.name(), self.server_version, self.instance_id);
                            let new_server = self.server.reload();
                            self.server_version = new_server.version();
                            self.server = new_server;
                            continue;
                        }
                        Ok(ServerReloadCommand::QuitRuntime) => {},
                        Err(RecvError::Closed) => {},
                        Err(RecvError::Lagged(dropped)) => {
                            warn!("SRT[{}_v{}#{}] server {} reload notify channel overflowed, {dropped} msg dropped",
                                self.server.name(), self.server_version, self.instance_id, self.server.name());
                            continue;
                        },
                    }

                    info!("SRT[{}_v{}#{}] will go offline",
                        self.server.name(), self.server_version, self.instance_id);
                    self.pre_stop();
                    let accept_again = listener.set_offline();
                    if accept_again {
                        info!("SRT[{}_v{}#{}] will accept all pending connections",
                            self.server.name(), self.server_version, self.instance_id);
                        continue;
                    } else {
                        break;
                    }
                }
                result = listener.accept() => {
                    if listener.accept_current_available(result, |result| {
                        match result {
                            Ok(Some((stream, peer_addr, local_addr))) => {
                                self.listen_stats.add_accepted();
                                self.run_task(
                                    stream,
                                    peer_addr.to_canonical(),
                                    local_addr.to_canonical(),
                                );
                                Ok(())
                            }
                            Ok(None) => {
                                info!("SRT[{}_v{}#{}] offline",
                                    self.server.name(), self.server_version, self.instance_id);
                                Err(())
                            }
                            Err(e) => {
                                self.listen_stats.add_failed();
                                warn!("SRT[{}_v{}#{}] accept: {e:?}",
                                    self.server.name(), self.server_version, self.instance_id);
                                Ok(())
                            }
                        }
                    }).await.is_err() {
                        break;
                    }
                }
            }
        }
        self.post_stop();
    }

    fn run_task(&self, stream: TcpStream, peer_addr: SocketAddr, local_addr: SocketAddr) {
        let server = self.server.clone();

        let mut cc_info = ClientConnectionInfo::new(peer_addr, local_addr);
        cc_info.set_tcp_raw_socket(RawSocket::from(&stream));
        if let Some(worker_id) = self.worker_id {
            cc_info.set_worker_id(Some(worker_id));
            tokio::spawn(async move {
                server.run_tcp_task(stream, cc_info).await;
            });
            return;
        }
        #[cfg(target_os = "linux")]
        if self.follow_incoming_cpu {
            if let Some(cpu_id) = cc_info.tcp_sock_incoming_cpu() {
                if let Some(rt) = crate::runtime::worker::select_handle_by_cpu_id(cpu_id) {
                    cc_info.set_worker_id(Some(rt.id));
                    rt.handle.spawn(async move {
                        server.run_tcp_task(stream, cc_info).await;
                    });
                    return;
                }
            }
        }
        if let Some(rt) = crate::runtime::worker::select_handle() {
            cc_info.set_worker_id(Some(rt.id));
            rt.handle.spawn(async move {
                server.run_tcp_task(stream, cc_info).await;
            });
        } else {
            tokio::spawn(async move {
                server.run_tcp_task(stream, cc_info).await;
            });
        }
    }

    fn get_rt_handle(&mut self, listen_in_worker: bool) -> (Handle, Option<CpuAffinity>) {
        if listen_in_worker {
            if let Some(rt) = crate::runtime::worker::select_listen_handle() {
                self.worker_id = Some(rt.id);
                return (rt.handle, rt.cpu_affinity);
            }
        }
        (Handle::current(), None)
    }

    fn into_running(
        mut self,
        listener: std::net::TcpListener,
        listen_in_worker: bool,
        follow_cpu_affinity: bool,
        server_reload_channel: broadcast::Receiver<ServerReloadCommand>,
    ) {
        let (handle, cpu_affinity) = self.get_rt_handle(listen_in_worker);
        handle.spawn(async move {
            if follow_cpu_affinity {
                #[cfg(target_os = "linux")]
                {
                    self.follow_incoming_cpu = true;
                }

                if let Some(cpu_affinity) = cpu_affinity {
                    if let Err(e) =
                        g3_socket::tcp::try_listen_on_local_cpu(&listener, &cpu_affinity)
                    {
                        warn!(
                            "SRT[{}_v{}#{}] failed to set cpu affinity for listen socket: {e}",
                            self.server.name(),
                            self.server_version,
                            self.instance_id
                        );
                    }
                }
            }
            // make sure the listen socket associated with the correct reactor
            match tokio::net::TcpListener::from_std(listener) {
                Ok(listener) => {
                    self.pre_start();
                    self.run(LimitedTcpListener::new(listener), server_reload_channel)
                        .await;
                }
                Err(e) => {
                    warn!(
                        "SRT[{}_v{}#{}] listen async: {e:?}",
                        self.server.name(),
                        self.server_version,
                        self.instance_id
                    );
                }
            }
        });
    }
}
