/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2025 ByteDance and/or its affiliates.
 */

use std::path::Path;

use anyhow::{Context, anyhow};
use yaml_rust::{Yaml, yaml};

use g3_macros::AnyConfig;
use g3_types::metrics::NodeName;
use g3_yaml::{HybridParser, YamlDocPosition};

mod registry;
pub(crate) use registry::{clear, get_all};

pub(crate) mod console;
pub(crate) mod discard;
pub(crate) mod graphite;
pub(crate) mod influxdb;
pub(crate) mod memory;
pub(crate) mod opentsdb;

const CONFIG_KEY_EXPORTER_TYPE: &str = "type";
const CONFIG_KEY_EXPORTER_NAME: &str = "name";

pub(crate) enum ExporterConfigDiffAction {
    NoAction,
    SpawnNew,
    Reload,
}

pub(crate) trait ExporterConfig {
    fn name(&self) -> &NodeName;
    fn position(&self) -> Option<YamlDocPosition>;
    fn exporter_type(&self) -> &'static str;

    fn diff_action(&self, new: &AnyExporterConfig) -> ExporterConfigDiffAction;
}

#[derive(Clone, Debug, AnyConfig)]
#[def_fn(name, &NodeName)]
#[def_fn(position, Option<YamlDocPosition>)]
#[def_fn(exporter_type, &'static str)]
#[def_fn(diff_action, &Self, ExporterConfigDiffAction)]
pub(crate) enum AnyExporterConfig {
    Discard(discard::DiscardExporterConfig),
    Console(console::ConsoleExporterConfig),
    Memory(memory::MemoryExporterConfig),
    Graphite(graphite::GraphiteExporterConfig),
    Opentsdb(opentsdb::OpentsdbExporterConfig),
    InfluxdbV2(influxdb::InfluxdbV2ExporterConfig),
    InfluxdbV3(influxdb::InfluxdbV3ExporterConfig),
}

pub(crate) fn load_all(v: &Yaml, conf_dir: &Path) -> anyhow::Result<()> {
    let parser = HybridParser::new(conf_dir, g3_daemon::opts::config_file_extension());
    parser.foreach_map(v, |map, position| {
        let exporter = load_exporter(map, position)?;
        if let Some(old_exporter) = registry::add(exporter) {
            Err(anyhow!(
                "exporter with name {} already exists",
                old_exporter.name()
            ))
        } else {
            Ok(())
        }
    })?;
    Ok(())
}

pub(crate) fn load_at_position(position: &YamlDocPosition) -> anyhow::Result<AnyExporterConfig> {
    let doc = g3_yaml::load_doc(position)?;
    if let Yaml::Hash(map) = doc {
        let exporter = load_exporter(&map, Some(position.clone()))?;
        registry::add(exporter.clone());
        Ok(exporter)
    } else {
        Err(anyhow!("yaml doc {position} is not a map"))
    }
}

fn load_exporter(
    map: &yaml::Hash,
    position: Option<YamlDocPosition>,
) -> anyhow::Result<AnyExporterConfig> {
    let exporter_type = g3_yaml::hash_get_required_str(map, CONFIG_KEY_EXPORTER_TYPE)?;
    match g3_yaml::key::normalize(exporter_type).as_str() {
        "discard" => {
            let exporter = discard::DiscardExporterConfig::parse(map, position)
                .context("failed to load this Discard exporter")?;
            Ok(AnyExporterConfig::Discard(exporter))
        }
        "console" => {
            let exporter = console::ConsoleExporterConfig::parse(map, position)
                .context("failed to load this Console exporter")?;
            Ok(AnyExporterConfig::Console(exporter))
        }
        "memory" => {
            let exporter = memory::MemoryExporterConfig::parse(map, position)
                .context("failed to load this Memory exporter")?;
            Ok(AnyExporterConfig::Memory(exporter))
        }
        "graphite" => {
            let exporter = graphite::GraphiteExporterConfig::parse(map, position)
                .context("failed to load this Graphite exporter")?;
            Ok(AnyExporterConfig::Graphite(exporter))
        }
        "opentsdb" => {
            let exporter = opentsdb::OpentsdbExporterConfig::parse(map, position)
                .context("failed to load this OpenTSDB exporter")?;
            Ok(AnyExporterConfig::Opentsdb(exporter))
        }
        "influxdb_v2" => {
            let exporter = influxdb::InfluxdbV2ExporterConfig::parse(map, position)
                .context("failed to load this InfluxDB v2 exporter")?;
            Ok(AnyExporterConfig::InfluxdbV2(exporter))
        }
        "influxdb_v3" => {
            let exporter = influxdb::InfluxdbV3ExporterConfig::parse(map, position)
                .context("failed to load this InfluxDB v3 exporter")?;
            Ok(AnyExporterConfig::InfluxdbV3(exporter))
        }
        _ => Err(anyhow!("unsupported exporter type {}", exporter_type)),
    }
}
