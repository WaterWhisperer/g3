/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2023-2025 ByteDance and/or its affiliates.
 */

use std::collections::HashSet;

use anyhow::{Context, anyhow};
use async_recursion::async_recursion;
use log::{debug, warn};
use tokio::sync::Mutex;

use g3_types::metrics::NodeName;
use g3_yaml::YamlDocPosition;

use super::registry;
use crate::config::escaper::{AnyEscaperConfig, EscaperConfigDiffAction};
use crate::escape::ArcEscaper;

use super::comply_audit::ComplyAuditEscaper;
use super::direct_fixed::DirectFixedEscaper;
use super::direct_float::DirectFloatEscaper;
use super::divert_tcp::DivertTcpEscaper;
use super::dummy_deny::DummyDenyEscaper;
use super::proxy_float::ProxyFloatEscaper;
use super::proxy_http::ProxyHttpEscaper;
use super::proxy_https::ProxyHttpsEscaper;
use super::proxy_socks5::ProxySocks5Escaper;
use super::proxy_socks5s::ProxySocks5sEscaper;
use super::route_client::RouteClientEscaper;
use super::route_failover::RouteFailoverEscaper;
use super::route_geoip::RouteGeoIpEscaper;
use super::route_mapping::RouteMappingEscaper;
use super::route_query::RouteQueryEscaper;
use super::route_resolved::RouteResolvedEscaper;
use super::route_select::RouteSelectEscaper;
use super::route_upstream::RouteUpstreamEscaper;
use super::trick_float::TrickFloatEscaper;

static ESCAPER_OPS_LOCK: Mutex<()> = Mutex::const_new(());

pub async fn load_all() -> anyhow::Result<()> {
    let _guard = ESCAPER_OPS_LOCK.lock().await;

    let mut new_names = HashSet::<NodeName>::new();

    let all_config = crate::config::escaper::get_all_sorted()?;
    for config in all_config {
        let name = config.name();
        new_names.insert(name.clone());
        match registry::get_config(name) {
            Some(old) => {
                debug!("reloading escaper {name}");
                reload_unlocked(old, config.as_ref().clone()).await?;
                debug!("escaper {name} reload OK");
            }
            None => {
                debug!("creating escaper {name}");
                spawn_new_unlocked(config.as_ref().clone()).await?;
                debug!("escaper {name} create OK");
            }
        }
    }

    for name in &registry::get_names() {
        if !new_names.contains(name) {
            debug!("deleting escaper {name}");
            delete_existed_unlocked(name).await;
            debug!("escaper {name} deleted");
        }
    }

    Ok(())
}

pub(crate) fn get_escaper(name: &NodeName) -> anyhow::Result<ArcEscaper> {
    match registry::get_escaper(name) {
        Some(server) => Ok(server),
        None => Err(anyhow!("no escaper named {name} found")),
    }
}

pub(crate) async fn reload(
    name: &NodeName,
    position: Option<YamlDocPosition>,
) -> anyhow::Result<()> {
    let _guard = ESCAPER_OPS_LOCK.lock().await;

    let old_config = match registry::get_config(name) {
        Some(config) => config,
        None => return Err(anyhow!("no escaper with name {name} found")),
    };

    let position = match position {
        Some(position) => position,
        None => match old_config.position() {
            Some(position) => position,
            None => {
                return Err(anyhow!(
                    "no config position for escaper {name} found, reload is not supported"
                ));
            }
        },
    };

    let position2 = position.clone();
    let config =
        tokio::task::spawn_blocking(move || crate::config::escaper::load_at_position(&position2))
            .await
            .map_err(|e| anyhow!("unable to join conf load task: {e}"))?
            .context(format!("unload to load conf at position {position}"))?;
    if name != config.name() {
        return Err(anyhow!(
            "escaper at position {position} has name {}, while we expect {name}",
            config.name()
        ));
    }

    debug!("reloading escaper {name} from position {position}");
    reload_unlocked(old_config, config).await?;
    debug!("escaper {name} reload OK");
    Ok(())
}

pub(crate) async fn update_dependency_to_resolver(resolver: &NodeName, status: &str) {
    let _guard = ESCAPER_OPS_LOCK.lock().await;

    let mut names = Vec::<NodeName>::new();

    registry::foreach(|name, escaper| {
        if escaper._resolver().eq(resolver) {
            names.push(name.clone());
        }
    });

    if names.is_empty() {
        return;
    }

    debug!("resolver {resolver} changed({status}), will reload escaper(s) {names:?}");
    for name in names.iter() {
        debug!("escaper {name}: will reload as it's using resolver {resolver}");
        if let Err(e) = reload_existed_unlocked(name, None).await {
            warn!("failed to reload escaper {name}: {e:?}");
        }
    }
}

pub(crate) async fn update_dependency_to_auditor(auditor: &NodeName, status: &str) {
    let _guard = ESCAPER_OPS_LOCK.lock().await;

    let mut names = Vec::<NodeName>::new();

    registry::foreach(|name, escaper| {
        if let Some(dep_auditor) = escaper._auditor() {
            if dep_auditor.eq(auditor) {
                names.push(name.clone());
            }
        }
    });

    if names.is_empty() {
        return;
    }

    debug!("auditor {auditor} changed({status}), will reload escaper(s) {names:?}");
    for name in names.iter() {
        debug!("escaper {name}: will reload as it's using auditor {auditor}");
        if let Err(e) = reload_existed_unlocked(name, None).await {
            warn!("failed to reload escaper {name}: {e:?}");
        }
    }
}

#[async_recursion]
async fn update_dependency_to_escaper_unlocked(target: &NodeName, status: &str) {
    let mut names = Vec::<NodeName>::new();

    registry::foreach(|name, escaper| {
        if escaper._depend_on_escaper(target) {
            names.push(name.clone());
        }
    });

    debug!(
        "escaper {target} changed({status}), will reload escaper(s) {names:?} which depend on it"
    );
    for name in names.iter() {
        debug!("escaper {name}: will reload as it depends on escaper {target}");
        if let Err(e) = reload_existed_unlocked(name, None).await {
            warn!("failed to reload escaper {name}: {e:?}");
        }
    }

    // finish those in the same level first, then go in depth
    for name in names.iter() {
        update_dependency_to_escaper_unlocked(name, "reloaded").await;
    }
}

async fn reload_unlocked(old: AnyEscaperConfig, new: AnyEscaperConfig) -> anyhow::Result<()> {
    let name = old.name();
    match old.diff_action(&new) {
        EscaperConfigDiffAction::NoAction => {
            debug!("escaper {name} reload: no action is needed");
            Ok(())
        }
        EscaperConfigDiffAction::SpawnNew => {
            debug!("escaper {name} reload: will create a totally new one");
            spawn_new_unlocked(new).await
        }
        EscaperConfigDiffAction::Reload => {
            debug!("escaper {name} reload: will reload from existed");
            reload_existed_unlocked(name, Some(new)).await
        }
    }
}

async fn delete_existed_unlocked(name: &NodeName) {
    const STATUS: &str = "deleted";

    registry::del(name);
    update_dependency_to_escaper_unlocked(name, STATUS).await;
    crate::serve::update_dependency_to_escaper(name, STATUS).await;
}

async fn reload_existed_unlocked(
    name: &NodeName,
    new: Option<AnyEscaperConfig>,
) -> anyhow::Result<()> {
    const STATUS: &str = "reloaded";

    registry::reload_existed(name, new)?;
    update_dependency_to_escaper_unlocked(name, STATUS).await;
    crate::serve::update_dependency_to_escaper(name, STATUS).await;
    Ok(())
}

async fn spawn_new_unlocked(config: AnyEscaperConfig) -> anyhow::Result<()> {
    const STATUS: &str = "spawned";

    let name = config.name().clone();
    let escaper = match config {
        AnyEscaperConfig::ComplyAudit(c) => ComplyAuditEscaper::prepare_initial(c)?,
        AnyEscaperConfig::DirectFixed(c) => DirectFixedEscaper::prepare_initial(c)?,
        AnyEscaperConfig::DirectFloat(c) => DirectFloatEscaper::prepare_initial(c).await?,
        AnyEscaperConfig::DivertTcp(c) => DivertTcpEscaper::prepare_initial(c)?,
        AnyEscaperConfig::DummyDeny(c) => DummyDenyEscaper::prepare_initial(c)?,
        AnyEscaperConfig::ProxyFloat(c) => ProxyFloatEscaper::prepare_initial(c).await?,
        AnyEscaperConfig::ProxyHttp(c) => ProxyHttpEscaper::prepare_initial(c)?,
        AnyEscaperConfig::ProxyHttps(c) => ProxyHttpsEscaper::prepare_initial(c)?,
        AnyEscaperConfig::ProxySocks5(c) => ProxySocks5Escaper::prepare_initial(c)?,
        AnyEscaperConfig::ProxySocks5s(c) => ProxySocks5sEscaper::prepare_initial(c)?,
        AnyEscaperConfig::RouteFailover(c) => RouteFailoverEscaper::prepare_initial(c)?,
        AnyEscaperConfig::RouteResolved(c) => RouteResolvedEscaper::prepare_initial(c)?,
        AnyEscaperConfig::RouteGeoIp(c) => RouteGeoIpEscaper::prepare_initial(c)?,
        AnyEscaperConfig::RouteMapping(c) => RouteMappingEscaper::prepare_initial(c)?,
        AnyEscaperConfig::RouteQuery(c) => RouteQueryEscaper::prepare_initial(c)?,
        AnyEscaperConfig::RouteSelect(c) => RouteSelectEscaper::prepare_initial(c)?,
        AnyEscaperConfig::RouteUpstream(c) => RouteUpstreamEscaper::prepare_initial(c)?,
        AnyEscaperConfig::RouteClient(c) => RouteClientEscaper::prepare_initial(c)?,
        AnyEscaperConfig::TrickFloat(c) => TrickFloatEscaper::prepare_initial(c)?,
    };
    registry::add(name.clone(), escaper);
    update_dependency_to_escaper_unlocked(&name, STATUS).await;
    crate::serve::update_dependency_to_escaper(&name, STATUS).await;
    Ok(())
}
