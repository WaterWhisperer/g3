/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2023-2025 ByteDance and/or its affiliates.
 */

use std::collections::HashMap;
use std::hash::Hash;
use std::num::{NonZeroI32, NonZeroIsize, NonZeroU32, NonZeroUsize};
use std::str::FromStr;

use anyhow::{Context, anyhow};
use ascii::AsciiString;
use yaml_rust::Yaml;

pub fn as_u8(v: &Yaml) -> anyhow::Result<u8> {
    match v {
        Yaml::String(s) => Ok(u8::from_str(s)?),
        Yaml::Integer(i) => Ok(u8::try_from(*i)?),
        _ => Err(anyhow!(
            "yaml value type for 'u8' should be 'string' or 'integer'"
        )),
    }
}

pub fn as_u16(v: &Yaml) -> anyhow::Result<u16> {
    match v {
        Yaml::String(s) => Ok(u16::from_str(s)?),
        Yaml::Integer(i) => Ok(u16::try_from(*i)?),
        _ => Err(anyhow!(
            "yaml value type for 'u16' should be 'string' or 'integer'"
        )),
    }
}

pub fn as_u32(v: &Yaml) -> anyhow::Result<u32> {
    match v {
        Yaml::String(s) => Ok(u32::from_str(s)?),
        Yaml::Integer(i) => Ok(u32::try_from(*i)?),
        _ => Err(anyhow!(
            "yaml value type for 'u32' should be 'string' or 'integer'"
        )),
    }
}

pub fn as_nonzero_u32(v: &Yaml) -> anyhow::Result<NonZeroU32> {
    match v {
        Yaml::String(s) => Ok(NonZeroU32::from_str(s)?),
        Yaml::Integer(i) => {
            let u = u32::try_from(*i)?;
            Ok(NonZeroU32::try_from(u)?)
        }
        _ => Err(anyhow!(
            "yaml value type for 'nonzero u32' should be 'string' or 'integer'"
        )),
    }
}

pub fn as_u64(v: &Yaml) -> anyhow::Result<u64> {
    match v {
        Yaml::String(s) => Ok(u64::from_str(s)?),
        Yaml::Integer(i) => Ok(u64::try_from(*i)?),
        _ => Err(anyhow!(
            "yaml value type for 'u64' should be 'string' or 'integer'"
        )),
    }
}

pub fn as_i32(v: &Yaml) -> anyhow::Result<i32> {
    match v {
        Yaml::String(s) => Ok(i32::from_str(s)?),
        Yaml::Integer(i) => Ok(i32::try_from(*i)?),
        _ => Err(anyhow!(
            "yaml value type for 'i32' should be 'string' or 'integer'"
        )),
    }
}

pub fn as_nonzero_i32(v: &Yaml) -> anyhow::Result<NonZeroI32> {
    match v {
        Yaml::String(s) => Ok(NonZeroI32::from_str(s)?),
        Yaml::Integer(i) => {
            let u = i32::try_from(*i)?;
            Ok(NonZeroI32::try_from(u)?)
        }
        _ => Err(anyhow!(
            "yaml value type for 'nonzero i32' should be 'string' or 'integer'"
        )),
    }
}

pub fn as_i64(v: &Yaml) -> anyhow::Result<i64> {
    match v {
        Yaml::String(s) => Ok(i64::from_str(s)?),
        Yaml::Integer(i) => Ok(*i),
        _ => Err(anyhow!(
            "yaml value type for 'i64' should be 'string' or 'integer'"
        )),
    }
}

pub fn as_f64(v: &Yaml) -> anyhow::Result<f64> {
    match v {
        Yaml::String(s) => Ok(f64::from_str(s)?),
        Yaml::Integer(i) => Ok(*i as f64),
        Yaml::Real(s) => Ok(f64::from_str(s)?),
        _ => Err(anyhow!(
            "yaml value type for 'f64' should be 'string', 'integer' or 'real'"
        )),
    }
}

pub fn as_bool(v: &Yaml) -> anyhow::Result<bool> {
    match v {
        Yaml::String(s) => match s.to_lowercase().as_str() {
            "on" | "true" | "yes" | "1" => Ok(true),
            "off" | "false" | "no" | "0" => Ok(false),
            _ => Err(anyhow!("invalid yaml string value for 'bool': {s}")),
        },
        Yaml::Boolean(value) => Ok(*value),
        Yaml::Integer(i) => Ok(*i != 0),
        _ => Err(anyhow!(
            "yaml value type for 'bool' should be 'boolean' / 'string' / 'integer'"
        )),
    }
}

pub fn as_nonzero_isize(v: &Yaml) -> anyhow::Result<NonZeroIsize> {
    match v {
        Yaml::String(s) => Ok(NonZeroIsize::from_str(s)?),
        Yaml::Integer(i) => {
            let u = isize::try_from(*i)?;
            Ok(NonZeroIsize::try_from(u)?)
        }
        _ => Err(anyhow!(
            "yaml value type for 'nonzero isize' should be 'string' or 'integer'"
        )),
    }
}

pub fn as_usize(v: &Yaml) -> anyhow::Result<usize> {
    match v {
        Yaml::String(s) => Ok(usize::from_str(s)?),
        Yaml::Integer(i) => Ok(usize::try_from(*i)?),
        _ => Err(anyhow!(
            "yaml value type for 'usize' should be 'string' or 'integer'"
        )),
    }
}

pub fn as_nonzero_usize(v: &Yaml) -> anyhow::Result<NonZeroUsize> {
    match v {
        Yaml::String(s) => Ok(NonZeroUsize::from_str(s)?),
        Yaml::Integer(i) => {
            let u = usize::try_from(*i)?;
            Ok(NonZeroUsize::try_from(u)?)
        }
        _ => Err(anyhow!(
            "yaml value type for 'nonzero usize' should be 'string' or 'integer'"
        )),
    }
}

pub fn as_ascii(v: &Yaml) -> anyhow::Result<AsciiString> {
    let s = as_string(v).context("the base type for AsciiString should be String")?;
    AsciiString::from_str(&s).map_err(|e| anyhow!("invalid ascii string: {e}"))
}

pub fn as_string(v: &Yaml) -> anyhow::Result<String> {
    match v {
        Yaml::String(s) => Ok(s.to_string()),
        Yaml::Integer(i) => Ok(i.to_string()),
        Yaml::Real(s) => Ok(s.to_string()),
        _ => Err(anyhow!(
            "yaml value type for string should be 'string' / 'integer' / 'real'"
        )),
    }
}

pub fn as_list<T, F>(v: &Yaml, convert: F) -> anyhow::Result<Vec<T>>
where
    F: Fn(&Yaml) -> anyhow::Result<T>,
{
    let mut vec = Vec::new();
    match v {
        Yaml::Array(seq) => {
            for (i, v) in seq.iter().enumerate() {
                let node = convert(v).context(format!("invalid value for list element #{i}"))?;
                vec.push(node);
            }
        }
        _ => {
            let node = convert(v).context("invalid single value for the list")?;
            vec.push(node);
        }
    }
    Ok(vec)
}

pub fn as_hashmap<K, V, KF, VF>(
    v: &Yaml,
    convert_key: KF,
    convert_value: VF,
) -> anyhow::Result<HashMap<K, V>>
where
    K: Hash + Eq,
    KF: Fn(&Yaml) -> anyhow::Result<K>,
    VF: Fn(&Yaml) -> anyhow::Result<V>,
{
    if let Yaml::Hash(map) = v {
        let mut table = HashMap::new();
        for (k, v) in map.iter() {
            let key = convert_key(k).context(format!("failed to parse key {k:?}"))?;
            let value = convert_value(v).context(format!("failed to parse value for key {k:?}"))?;
            table.insert(key, value);
        }
        Ok(table)
    } else {
        Err(anyhow!("the yaml value should be a 'map'"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t_string() {
        let v = Yaml::String("123.0".to_string());
        let pv = as_string(&v).unwrap();
        assert_eq!(pv, "123.0");

        let v = Yaml::Integer(123);
        let pv = as_string(&v).unwrap();
        assert_eq!(pv, "123");

        let v = Yaml::Integer(-123);
        let pv = as_string(&v).unwrap();
        assert_eq!(pv, "-123");

        let v = Yaml::Real("123.0".to_string());
        let pv = as_string(&v).unwrap();
        assert_eq!(pv, "123.0");
    }
}
