/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2023-2025 ByteDance and/or its affiliates.
 */

use std::sync::Arc;
use std::time::Duration;

use capnp::capability::Promise;
use capnp_rpc::pry;

use g3_types::metrics::NodeName;
use g3_types::resolve::{QueryStrategy as ResolveQueryStrategy, ResolveStrategy};

use g3proxy_proto::resolver_capnp::{QueryStrategy, resolver_control};

use crate::resolve::{ArcIntegratedResolverHandle, HappyEyeballsResolveJob};

pub(super) struct ResolverControlImpl {
    resolver_handler: ArcIntegratedResolverHandle,
}

impl ResolverControlImpl {
    pub(super) fn new_client(name: &str) -> anyhow::Result<resolver_control::Client> {
        let name = unsafe { NodeName::new_unchecked(name) };
        let handler = crate::resolve::get_handle(&name)?;
        Ok(capnp_rpc::new_client(ResolverControlImpl {
            resolver_handler: handler,
        }))
    }
}

impl resolver_control::Server for ResolverControlImpl {
    fn query(
        &mut self,
        params: resolver_control::QueryParams,
        mut results: resolver_control::QueryResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let domain = pry!(pry!(params.get_domain()).to_string());
        let resolution_delay = params.get_resolution_delay() as u64;
        let query_strategy = pry!(params.get_strategy());
        let resolver_strategy = get_resolver_strategy(query_strategy);
        let resolver_handler = Arc::clone(&self.resolver_handler);

        Promise::from_future(async move {
            let mut job = match HappyEyeballsResolveJob::new_dyn(
                resolver_strategy,
                &resolver_handler,
                Arc::from(domain),
            ) {
                Ok(job) => job,
                Err(e) => {
                    results
                        .get()
                        .init_result()
                        .set_err(format!("failed to create resolve job: {e:?}").as_str());
                    return Ok(());
                }
            };
            match job
                .get_r1_or_first(Duration::from_millis(resolution_delay), usize::MAX)
                .await
            {
                Ok(ips) => {
                    let mut ips_builder = results.get().init_result().init_ip(ips.len() as u32);
                    for (i, ip) in ips.iter().enumerate() {
                        ips_builder.set(i as u32, ip.to_string().as_str());
                    }
                }
                Err(e) => results
                    .get()
                    .init_result()
                    .set_err(format!("{e:?}").as_str()),
            }
            Ok(())
        })
    }
}

fn get_resolver_strategy(q: QueryStrategy) -> ResolveStrategy {
    let qs = match q {
        QueryStrategy::Ipv4First => ResolveQueryStrategy::Ipv4First,
        QueryStrategy::Ipv6First => ResolveQueryStrategy::Ipv6First,
        QueryStrategy::Ipv4Only => ResolveQueryStrategy::Ipv4Only,
        QueryStrategy::Ipv6Only => ResolveQueryStrategy::Ipv6Only,
    };
    ResolveStrategy {
        query: qs,
        pick: Default::default(),
    }
}
