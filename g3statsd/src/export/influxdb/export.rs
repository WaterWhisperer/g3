/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2025 ByteDance and/or its affiliates.
 */

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use ahash::AHashMap;
use anyhow::anyhow;
use chrono::{DateTime, Utc};
use http::uri::PathAndQuery;
use http::{HeaderMap, HeaderValue, header};
use itoa::Buffer;
use tokio::sync::mpsc;

use g3_http::client::HttpForwardRemoteResponse;
use g3_types::metrics::MetricTagMap;

use crate::config::exporter::influxdb::{InfluxdbExporterConfig, TimestampPrecision};
use crate::runtime::export::{AggregateExport, CounterStoreValue, GaugeStoreValue, HttpExport};
use crate::types::{MetricName, MetricValue};

pub(super) struct InfluxdbEncodedLines {
    len: usize,
    buf: Vec<u8>,
}

pub(super) struct InfluxdbAggregateExport {
    emit_interval: Duration,
    precision: TimestampPrecision,
    max_body_lines: usize,
    prefix: Option<MetricName>,
    global_tags: MetricTagMap,
    lines_sender: mpsc::UnboundedSender<InfluxdbEncodedLines>,

    buf: Vec<u8>,
}

impl InfluxdbAggregateExport {
    pub(super) fn new<T: InfluxdbExporterConfig>(
        config: &T,
        lines_sender: mpsc::UnboundedSender<InfluxdbEncodedLines>,
    ) -> Self {
        InfluxdbAggregateExport {
            emit_interval: config.emit_interval(),
            precision: config.precision(),
            max_body_lines: config.max_body_lines(),
            prefix: config.prefix(),
            global_tags: config.global_tags(),
            lines_sender,
            buf: Vec::new(),
        }
    }

    fn serialize_name_tags(&mut self, name: &MetricName, tag_map: &MetricTagMap) {
        if let Some(prefix) = &self.prefix {
            let _ = write!(
                &mut self.buf,
                "{}.{}",
                prefix.display('.'),
                name.display('.')
            );
        } else {
            let _ = write!(&mut self.buf, "{}", name.display('.'));
        }
        if !self.global_tags.is_empty() {
            let _ = write!(&mut self.buf, ",{}", self.global_tags.display_influxdb());
        }
        if !tag_map.is_empty() {
            let _ = write!(&mut self.buf, ",{}", tag_map.display_influxdb());
        }
    }

    fn serialize_timestamp(&mut self, time: &DateTime<Utc>) {
        let mut ts_buffer = Buffer::new();
        match self.precision {
            TimestampPrecision::Seconds => {
                let ts = ts_buffer.format(time.timestamp());
                self.buf.push(b' ');
                self.buf.extend_from_slice(ts.as_bytes());
            }
            TimestampPrecision::MilliSeconds => {
                let ts = ts_buffer.format(time.timestamp_millis());
                self.buf.push(b' ');
                self.buf.extend_from_slice(ts.as_bytes());
            }
            TimestampPrecision::MicroSeconds => {
                let ts = ts_buffer.format(time.timestamp_micros());
                self.buf.push(b' ');
                self.buf.extend_from_slice(ts.as_bytes());
            }
            TimestampPrecision::NanoSeconds => {
                if let Some(ts_nanos) = time.timestamp_nanos_opt() {
                    let ts = ts_buffer.format(ts_nanos);
                    self.buf.push(b' ');
                    self.buf.extend_from_slice(ts.as_bytes());
                }
            }
        };
    }

    fn send_lines(&mut self, line_number: usize) {
        if line_number == 0 || self.buf.is_empty() {
            return;
        }
        let _ = self.lines_sender.send(InfluxdbEncodedLines {
            len: line_number,
            buf: self.buf.clone(),
        });
        self.buf.clear();
    }
}

impl AggregateExport for InfluxdbAggregateExport {
    fn emit_interval(&self) -> Duration {
        self.emit_interval
    }

    fn emit_gauge(
        &mut self,
        name: &MetricName,
        values: &AHashMap<Arc<MetricTagMap>, GaugeStoreValue>,
    ) {
        let mut line_number = 0;
        self.buf.clear();

        for (tag_map, gauge) in values {
            self.serialize_name_tags(name, tag_map);

            let _ = write!(&mut self.buf, " value={}", gauge.value.display_influxdb());

            self.serialize_timestamp(&gauge.time);
            self.buf.push(b'\n');

            line_number += 1;
            if line_number >= self.max_body_lines {
                self.send_lines(line_number);
                line_number = 0;
            }
        }

        self.send_lines(line_number);
    }

    fn emit_counter(
        &mut self,
        name: &MetricName,
        values: &AHashMap<Arc<MetricTagMap>, CounterStoreValue>,
    ) {
        let mut line_number = 0;
        self.buf.clear();

        for (tag_map, counter) in values {
            self.serialize_name_tags(name, tag_map);

            let rate =
                MetricValue::Double(counter.diff.as_f64() / self.emit_interval.as_secs_f64());
            let _ = write!(
                &mut self.buf,
                " count={},diff={},rate={}",
                counter.sum.display_influxdb(),
                counter.diff.display_influxdb(),
                rate.display_influxdb(),
            );

            self.serialize_timestamp(&counter.time);
            self.buf.push(b'\n');

            line_number += 1;
            if line_number >= self.max_body_lines {
                self.send_lines(line_number);
                line_number = 0;
            }
        }

        self.send_lines(line_number);
    }
}

pub(super) struct InfluxdbHttpExport {
    api_path: PathAndQuery,
    static_headers: HeaderMap,
    max_body_lines: usize,
}

impl InfluxdbHttpExport {
    pub(super) fn new<T: InfluxdbExporterConfig>(config: &T) -> anyhow::Result<Self> {
        let api_path = config.build_api_path()?;
        let mut static_headers = HeaderMap::new();
        static_headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        static_headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        if let Some(v) = config.build_api_token() {
            static_headers.insert(header::AUTHORIZATION, v);
        }
        Ok(InfluxdbHttpExport {
            api_path,
            static_headers,
            max_body_lines: config.max_body_lines(),
        })
    }
}

// https://docs.influxdata.com/influxdb3/core/write-data/api-client-libraries/
impl HttpExport for InfluxdbHttpExport {
    type BodyPiece = InfluxdbEncodedLines;

    fn api_path(&self) -> &PathAndQuery {
        &self.api_path
    }

    fn static_headers(&self) -> &HeaderMap {
        &self.static_headers
    }

    fn fill_body(&mut self, pieces: &[InfluxdbEncodedLines], body_buf: &mut Vec<u8>) -> usize {
        let mut added_lines = 0;
        let mut handled_pieces = 0;
        for piece in pieces {
            if added_lines + piece.len > self.max_body_lines {
                return handled_pieces;
            }

            body_buf.extend_from_slice(&piece.buf);
            handled_pieces += 1;
            added_lines += piece.len;
        }
        handled_pieces
    }

    fn check_response(&self, rsp: HttpForwardRemoteResponse, body: &[u8]) -> anyhow::Result<()> {
        if rsp.code != 200 && rsp.code != 204 {
            if let Ok(detail) = std::str::from_utf8(body) {
                Err(anyhow!("error response: {} {detail}", rsp.code))
            } else {
                Err(anyhow!("error response: {}", rsp.code))
            }
        } else {
            Ok(())
        }
    }
}
