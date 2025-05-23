/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2023-2025 ByteDance and/or its affiliates.
 */

use std::collections::hash_map;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use ahash::AHashMap;
use log::{trace, warn};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tokio_util::time::{DelayQueue, delay_queue};

use super::stats::{ResolverMemoryStats, ResolverStats};
use super::{ArcResolvedRecord, BoxResolverDriver, ResolvedRecordSource, ResolverConfig};
use crate::message::{ResolveDriverRequest, ResolveDriverResponse, ResolverCommand};

struct CachedRecord {
    inner: ArcResolvedRecord,
    expire_at: Instant,
    expire_key: Option<delay_queue::Key>,
}

struct TrashedRecord {
    inner: ArcResolvedRecord,
    vanish_at: Instant,
}

pub(crate) struct ResolverRuntime {
    config: ResolverConfig,
    stats: Arc<ResolverStats>,
    req_receiver: mpsc::UnboundedReceiver<ResolveDriverRequest>,
    ctl_receiver: mpsc::UnboundedReceiver<ResolverCommand>,
    rsp_receiver: mpsc::UnboundedReceiver<ResolveDriverResponse>,
    rsp_sender: mpsc::UnboundedSender<ResolveDriverResponse>,
    expired_v4: DelayQueue<Arc<str>>,
    expired_v6: DelayQueue<Arc<str>>,
    cache_v4: AHashMap<Arc<str>, CachedRecord>,
    cache_v6: AHashMap<Arc<str>, CachedRecord>,
    doing_v4: AHashMap<Arc<str>, Vec<oneshot::Sender<(ArcResolvedRecord, ResolvedRecordSource)>>>,
    doing_v6: AHashMap<Arc<str>, Vec<oneshot::Sender<(ArcResolvedRecord, ResolvedRecordSource)>>>,
    trash_v4: AHashMap<Arc<str>, TrashedRecord>,
    trash_v6: AHashMap<Arc<str>, TrashedRecord>,
    driver: Option<BoxResolverDriver>,
}

impl Drop for ResolverRuntime {
    fn drop(&mut self) {
        self.req_receiver.close();
        self.rsp_receiver.close();
    }
}

impl ResolverRuntime {
    pub(crate) fn new(
        config: ResolverConfig,
        req_receiver: mpsc::UnboundedReceiver<ResolveDriverRequest>,
        ctl_receiver: mpsc::UnboundedReceiver<ResolverCommand>,
        stats: Arc<ResolverStats>,
    ) -> Self {
        let initial_cache_capacity = config.runtime.initial_cache_capacity;
        let (rsp_sender, rsp_receiver) = mpsc::unbounded_channel();
        ResolverRuntime {
            config,
            stats,
            req_receiver,
            ctl_receiver,
            rsp_receiver,
            rsp_sender,
            expired_v4: DelayQueue::with_capacity(initial_cache_capacity),
            expired_v6: DelayQueue::with_capacity(initial_cache_capacity),
            cache_v4: AHashMap::with_capacity(initial_cache_capacity),
            cache_v6: AHashMap::with_capacity(initial_cache_capacity),
            doing_v4: AHashMap::with_capacity(initial_cache_capacity),
            doing_v6: AHashMap::with_capacity(initial_cache_capacity),
            trash_v4: AHashMap::with_capacity(initial_cache_capacity),
            trash_v6: AHashMap::with_capacity(initial_cache_capacity),
            driver: None,
        }
    }

    fn handle_cmd(&mut self, cmd: ResolverCommand) {
        match cmd {
            ResolverCommand::Update(config) => match config.driver.spawn_resolver_driver() {
                Ok(driver) => {
                    self.driver = Some(driver);
                    self.config = *config;
                }
                Err(e) => {
                    warn!("invalid resolver config {config:?} : {e}");
                }
            },
            ResolverCommand::Quit => {} // should be handled outside
        }
    }

    fn update_cache(
        cache: &mut AHashMap<Arc<str>, CachedRecord>,
        expire_queue: &mut DelayQueue<Arc<str>>,
        record: ArcResolvedRecord,
        expire_at: Instant,
    ) {
        match cache.entry(record.domain.clone()) {
            hash_map::Entry::Occupied(mut o) => {
                let v = o.get_mut();
                let expire_key = match v.expire_key.take() {
                    Some(expire_key) => {
                        expire_queue.reset_at(&expire_key, expire_at);
                        expire_key
                    }
                    None => expire_queue.insert_at(record.domain.clone(), expire_at),
                };
                v.inner = record;
                v.expire_at = expire_at;
                v.expire_key = Some(expire_key);
            }
            hash_map::Entry::Vacant(v) => {
                let expire_key = expire_queue.insert_at(record.domain.to_owned(), expire_at);
                v.insert(CachedRecord {
                    inner: record,
                    expire_at,
                    expire_key: Some(expire_key),
                });
            }
        }
    }

    fn handle_rsp(&mut self, rsp: ResolveDriverResponse) {
        match rsp {
            ResolveDriverResponse::V4(record) => {
                self.stats.query_a.add_record(&record);
                if !record.is_acceptable() {
                    if let Some(v) = self.trash_v4.get(&record.domain) {
                        if let Some(vec) = self.doing_v4.remove(&record.domain) {
                            self.stats.query_a.add_query_trashed_n(vec.len());
                            for sender in vec.into_iter() {
                                let _ = sender.send((v.inner.clone(), ResolvedRecordSource::Trash));
                            }
                        }
                        return;
                    }
                } else {
                    self.trash_v4.remove(&record.domain);
                }
                let record = Arc::new(record);
                if let Some(mut vec) = self.doing_v4.remove(&record.domain) {
                    if let Some(sender) = vec.pop() {
                        let _ = sender.send((Arc::clone(&record), ResolvedRecordSource::Query));
                        self.stats.query_a.add_query_cached_n(vec.len());
                        for sender in vec.into_iter() {
                            let _ = sender.send((Arc::clone(&record), ResolvedRecordSource::Cache));
                        }
                    }
                }
                if let Some(expire_at) = record.expire {
                    Self::update_cache(&mut self.cache_v4, &mut self.expired_v4, record, expire_at);
                }
            }
            ResolveDriverResponse::V6(record) => {
                self.stats.query_aaaa.add_record(&record);
                if !record.is_acceptable() {
                    if let Some(v) = self.trash_v6.get(&record.domain) {
                        if let Some(vec) = self.doing_v6.remove(&record.domain) {
                            self.stats.query_aaaa.add_query_trashed_n(vec.len());
                            for sender in vec.into_iter() {
                                let _ = sender.send((v.inner.clone(), ResolvedRecordSource::Trash));
                            }
                        }
                        return;
                    }
                } else {
                    self.trash_v6.remove(&record.domain);
                }
                let record = Arc::new(record);
                if let Some(mut vec) = self.doing_v6.remove(&record.domain) {
                    if let Some(sender) = vec.pop() {
                        let _ = sender.send((Arc::clone(&record), ResolvedRecordSource::Query));
                        self.stats.query_aaaa.add_query_cached_n(vec.len());
                        for sender in vec.into_iter() {
                            let _ = sender.send((Arc::clone(&record), ResolvedRecordSource::Cache));
                        }
                    }
                }
                if let Some(expire_at) = record.expire {
                    Self::update_cache(&mut self.cache_v6, &mut self.expired_v6, record, expire_at);
                }
            }
        }
    }

    fn handle_expired_v4(&mut self, domain: &str) {
        trace!("clean expired v4 for domain {domain}");
        if let Some(r) = self.cache_v4.remove(domain) {
            if let Some(vanish_at) = r.inner.vanish {
                self.trash_v4.insert(
                    r.inner.domain.clone(),
                    TrashedRecord {
                        inner: r.inner,
                        vanish_at,
                    },
                );
            }
        }
    }
    fn handle_expired_v6(&mut self, domain: &str) {
        trace!("clean expired v6 for domain {domain}");
        if let Some(r) = self.cache_v6.remove(domain) {
            if let Some(vanish_at) = r.inner.vanish {
                self.trash_v6.insert(
                    r.inner.domain.clone(),
                    TrashedRecord {
                        inner: r.inner,
                        vanish_at,
                    },
                );
            }
        }
    }

    fn handle_req(&mut self, req: ResolveDriverRequest) {
        match req {
            ResolveDriverRequest::GetV4(domain, sender) => {
                self.stats.query_a.add_query_total();
                if let Some(r) = self.cache_v4.get(&domain) {
                    self.stats.query_a.add_query_cached();
                    let _ = sender.send((Arc::clone(&r.inner), ResolvedRecordSource::Cache));
                    return;
                }
                if let Some(r) = self.trash_v4.get(&domain) {
                    self.stats.query_a.add_query_trashed();
                    let _ = sender.send((Arc::clone(&r.inner), ResolvedRecordSource::Trash));
                    self.doing_v4.entry(domain.clone()).or_insert_with(|| {
                        if let Some(driver) = &self.driver {
                            self.stats.query_a.add_query_driver();
                            driver.query_v4(domain, &self.config.runtime, self.rsp_sender.clone());
                        }
                        vec![]
                    });
                    return;
                }
                match self.doing_v4.entry(domain.clone()) {
                    hash_map::Entry::Occupied(mut o) => {
                        // there is a query already
                        o.get_mut().push(sender);
                    }
                    hash_map::Entry::Vacant(v) => {
                        v.insert(vec![sender]);
                        if let Some(driver) = &self.driver {
                            self.stats.query_a.add_query_driver();
                            driver.query_v4(domain, &self.config.runtime, self.rsp_sender.clone());
                        } else {
                            unreachable!()
                        }
                    }
                }
            }
            ResolveDriverRequest::GetV6(domain, sender) => {
                self.stats.query_aaaa.add_query_total();
                if let Some(r) = self.cache_v6.get(&domain) {
                    self.stats.query_aaaa.add_query_cached();
                    let _ = sender.send((Arc::clone(&r.inner), ResolvedRecordSource::Cache));
                    return;
                }
                if let Some(r) = self.trash_v6.get(&domain) {
                    self.stats.query_aaaa.add_query_trashed();
                    let _ = sender.send((Arc::clone(&r.inner), ResolvedRecordSource::Trash));
                    self.doing_v6.entry(domain.clone()).or_insert_with(|| {
                        if let Some(driver) = &self.driver {
                            self.stats.query_aaaa.add_query_driver();
                            driver.query_v6(domain, &self.config.runtime, self.rsp_sender.clone());
                        }
                        vec![]
                    });
                    return;
                }
                match self.doing_v6.entry(domain.clone()) {
                    hash_map::Entry::Occupied(mut o) => {
                        // there is a query already
                        o.get_mut().push(sender);
                    }
                    hash_map::Entry::Vacant(v) => {
                        v.insert(vec![sender]);
                        if let Some(driver) = &self.driver {
                            self.stats.query_aaaa.add_query_driver();
                            driver.query_v6(domain, &self.config.runtime, self.rsp_sender.clone());
                        } else {
                            unreachable!()
                        }
                    }
                }
            }
        }
    }

    fn update_mem_stats(&self) {
        fn update<K, VC, VD, VT>(
            stats: &ResolverMemoryStats,
            cache_ht: &AHashMap<K, VC>,
            doing_ht: &AHashMap<K, VD>,
            trash_ht: &AHashMap<K, VT>,
        ) {
            stats.set_cache_capacity(cache_ht.capacity());
            stats.set_cache_length(cache_ht.len());
            stats.set_doing_capacity(doing_ht.capacity());
            stats.set_doing_length(doing_ht.len());
            stats.set_trash_capacity(trash_ht.capacity());
            stats.set_trash_length(trash_ht.len());
        }

        update(
            &self.stats.memory_a,
            &self.cache_v4,
            &self.doing_v4,
            &self.trash_v4,
        );
        update(
            &self.stats.memory_aaaa,
            &self.cache_v6,
            &self.doing_v6,
            &self.trash_v6,
        );
    }

    fn clean_trash(&mut self) {
        let now = Instant::now();
        self.trash_v4.retain(|_, v| v.vanish_at > now);
        self.trash_v6.retain(|_, v| v.vanish_at > now);
    }

    fn poll_loop(&mut self, cx: &mut Context<'_>) -> Poll<anyhow::Result<()>> {
        if self.driver.is_none() {
            self.driver = Some(self.config.driver.spawn_resolver_driver()?);
        }

        self.clean_trash();

        'outer: loop {
            // handle command
            let cmd = match self.ctl_receiver.poll_recv(cx) {
                Poll::Pending => None,
                Poll::Ready(Some(cmd)) => Some(cmd),
                Poll::Ready(None) => break, // sender closed
            };
            if let Some(cmd) = cmd {
                if matches!(cmd, ResolverCommand::Quit) {
                    break;
                } else {
                    self.handle_cmd(cmd);
                }
            }

            let mut update_mem_stats = false;

            // handle response
            loop {
                let rsp = match self.rsp_receiver.poll_recv(cx) {
                    Poll::Pending => break,
                    Poll::Ready(Some(rsp)) => rsp,
                    Poll::Ready(None) => unreachable!(), // unreachable as we have kept a sender
                };
                update_mem_stats = true;
                self.handle_rsp(rsp);
            }

            // handle expired
            loop {
                match self.expired_v4.poll_expired(cx) {
                    Poll::Pending => break,
                    Poll::Ready(None) => break, // all items fetched
                    Poll::Ready(Some(t)) => {
                        update_mem_stats = true;
                        self.handle_expired_v4(t.get_ref());
                    }
                }
            }
            loop {
                match self.expired_v6.poll_expired(cx) {
                    Poll::Pending => break,
                    Poll::Ready(None) => break, // all items fetched
                    Poll::Ready(Some(t)) => {
                        update_mem_stats = true;
                        self.handle_expired_v6(t.get_ref());
                    }
                }
            }

            if update_mem_stats {
                self.update_mem_stats();
            }

            // handle request
            for _ in 1..self.config.runtime.batch_request_count {
                let req = match self.req_receiver.poll_recv(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(req) => req,
                };
                match req {
                    // use another match to avoid multiple borrow of self
                    Some(req) => self.handle_req(req),
                    None => break 'outer,
                }
            }
        }

        Poll::Ready(Ok(()))
    }
}

impl Future for ResolverRuntime {
    type Output = anyhow::Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        (*self).poll_loop(cx)
    }
}
