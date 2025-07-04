/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2023-2025 ByteDance and/or its affiliates.
 */

use std::net::{IpAddr, SocketAddr};
#[cfg(unix)]
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, anyhow};
use log::warn;
use yaml_rust::Yaml;

use g3_types::metrics::NodeName;

use super::{StatsdBackend, StatsdClientConfig};

impl StatsdBackend {
    pub fn parse_udp_yaml(v: &Yaml) -> anyhow::Result<Self> {
        match v {
            Yaml::Hash(map) => {
                let mut addr: Option<SocketAddr> = None;
                let mut bind: Option<IpAddr> = None;

                g3_yaml::foreach_kv(map, |k, v| match g3_yaml::key::normalize(k).as_str() {
                    "address" | "addr" => {
                        addr = Some(g3_yaml::value::as_env_sockaddr(v).context(format!(
                            "invalid statsd udp peer socket address value for key {k}"
                        ))?);
                        Ok(())
                    }
                    "bind_ip" | "bind" => {
                        bind = Some(
                            g3_yaml::value::as_ipaddr(v)
                                .context(format!("invalid value for key {k}"))?,
                        );
                        Ok(())
                    }
                    _ => Err(anyhow!("invalid key {k}")),
                })?;

                if let Some(addr) = addr.take() {
                    Ok(StatsdBackend::Udp(addr, bind))
                } else {
                    Err(anyhow!("no target address has been set"))
                }
            }
            Yaml::String(s) => {
                let addr =
                    SocketAddr::from_str(s).map_err(|e| anyhow!("invalid SocketAddr: {e}"))?;
                Ok(StatsdBackend::Udp(addr, None))
            }
            _ => Err(anyhow!("invalid yaml value for udp statsd backend")),
        }
    }

    #[cfg(unix)]
    pub fn parse_unix_yaml(v: &Yaml) -> anyhow::Result<Self> {
        match v {
            Yaml::Hash(map) => {
                let mut path: Option<PathBuf> = None;

                g3_yaml::foreach_kv(map, |k, v| match g3_yaml::key::normalize(k).as_str() {
                    "path" => {
                        path = Some(
                            g3_yaml::value::as_absolute_path(v)
                                .context(format!("invalid value for key {k}"))?,
                        );
                        Ok(())
                    }
                    _ => Err(anyhow!("invalid key {k}")),
                })?;
                if let Some(path) = path.take() {
                    Ok(StatsdBackend::Unix(path))
                } else {
                    Err(anyhow!("no path has been set"))
                }
            }
            Yaml::String(_) => {
                let path = g3_yaml::value::as_absolute_path(v)?;
                Ok(StatsdBackend::Unix(path))
            }
            _ => Err(anyhow!("invalid yaml value for unix statsd backend")),
        }
    }
}

impl StatsdClientConfig {
    pub fn parse_yaml(v: &Yaml, prefix: NodeName) -> anyhow::Result<Self> {
        if let Yaml::Hash(map) = v {
            let mut config = StatsdClientConfig::with_prefix(prefix);
            g3_yaml::foreach_kv(map, |k, v| config.set_by_yaml_kv(k, v))?;
            Ok(config)
        } else {
            Err(anyhow!(
                "yaml value type for 'statsd client config' should be 'map'"
            ))
        }
    }

    fn set_by_yaml_kv(&mut self, k: &str, v: &Yaml) -> anyhow::Result<()> {
        match g3_yaml::key::normalize(k).as_str() {
            "target_udp" | "backend_udp" => {
                let target = StatsdBackend::parse_udp_yaml(v)
                    .context(format!("invalid value for key {k}"))?;
                self.set_backend(target);
            }
            #[cfg(unix)]
            "target_unix" | "backend_unix" => {
                let target = StatsdBackend::parse_unix_yaml(v)
                    .context(format!("invalid value for key {k}"))?;
                self.set_backend(target);
            }
            "target" | "backend" => {
                return if let Yaml::Hash(map) = v {
                    g3_yaml::foreach_kv(map, |k, v| match g3_yaml::key::normalize(k).as_str() {
                        "udp" => {
                            let target = StatsdBackend::parse_udp_yaml(v)
                                .context(format!("invalid value for key {k}"))?;
                            self.set_backend(target);
                            Ok(())
                        }
                        #[cfg(unix)]
                        "unix" => {
                            let target = StatsdBackend::parse_unix_yaml(v)
                                .context(format!("invalid value for key {k}"))?;
                            self.set_backend(target);
                            Ok(())
                        }
                        _ => Err(anyhow!("invalid key {k}")),
                    })
                    .context(format!("invalid value for key {k}"))
                } else {
                    Err(anyhow!("yaml value type for key {k} should be 'map'"))
                };
            }
            "prefix" => {
                let prefix = g3_yaml::value::as_metric_node_name(v)
                    .context(format!("invalid metrics name value for key {k}"))?;
                self.set_prefix(prefix);
            }
            "cache_size" => {
                self.cache_size = g3_yaml::humanize::as_usize(v)
                    .context(format!("invalid humanize usize value for key {k}"))?;
            }
            "max_segment_size" => {
                let size = g3_yaml::humanize::as_usize(v)
                    .context(format!("invalid humanize usize value for key {k}"))?;
                self.max_segment_size = Some(size);
            }
            "emit_duration" => {
                warn!("deprecated config key '{k}', please use 'emit_interval' instead");
                return self.set_by_yaml_kv("emit_interval", v);
            }
            "emit_interval" => {
                self.emit_interval = g3_yaml::humanize::as_duration(v)
                    .context(format!("invalid humanize duration value for key {k}"))?;
            }
            _ => return Err(anyhow!("invalid key {k}")),
        }
        Ok(())
    }
}
