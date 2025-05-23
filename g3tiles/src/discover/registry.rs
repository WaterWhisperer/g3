/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2023-2025 ByteDance and/or its affiliates.
 */

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use anyhow::anyhow;
use foldhash::fast::FixedState;

use g3_types::metrics::NodeName;

use super::ArcDiscoverInternal;
use crate::config::discover::AnyDiscoverConfig;

static RUNTIME_DISCOVER_REGISTRY: Mutex<HashMap<NodeName, ArcDiscoverInternal, FixedState>> =
    Mutex::new(HashMap::with_hasher(FixedState::with_seed(0)));

pub(super) fn add(name: NodeName, discover: ArcDiscoverInternal) {
    let mut ht = RUNTIME_DISCOVER_REGISTRY.lock().unwrap();
    if let Some(_old) = ht.insert(name, discover) {}
}

pub(crate) fn get(name: &NodeName) -> Option<ArcDiscoverInternal> {
    let ht = RUNTIME_DISCOVER_REGISTRY.lock().unwrap();
    ht.get(name).cloned()
}

pub(super) fn del(name: &NodeName) {
    let mut ht = RUNTIME_DISCOVER_REGISTRY.lock().unwrap();
    if let Some(_old) = ht.remove(name) {}
}

pub(crate) fn get_names() -> HashSet<NodeName> {
    let mut names = HashSet::new();
    let ht = RUNTIME_DISCOVER_REGISTRY.lock().unwrap();
    for key in ht.keys() {
        names.insert(key.clone());
    }
    names
}

pub(super) fn get_config(name: &NodeName) -> Option<AnyDiscoverConfig> {
    let ht = RUNTIME_DISCOVER_REGISTRY.lock().unwrap();
    ht.get(name).map(|g| g._clone_config())
}

pub(super) fn update_config_in_place(
    name: &NodeName,
    config: AnyDiscoverConfig,
) -> anyhow::Result<()> {
    if let Some(discover) = get(name) {
        discover._update_config_in_place(config)
    } else {
        Err(anyhow!("no discover with name {name} found"))
    }
}
