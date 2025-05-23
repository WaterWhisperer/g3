/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2023-2025 ByteDance and/or its affiliates.
 */

use std::io::Write;
use std::str::FromStr;

use bytes::{BufMut, Bytes, BytesMut};
use http::{HeaderName, Method, Version, header};
use tokio::io::AsyncBufRead;

use g3_io_ext::LimitedBufReadExt;
use g3_types::net::{HttpHeaderMap, HttpHeaderValue, HttpUpgradeToken};

use super::{HttpAdaptedResponse, HttpResponseParseError};
use crate::header::Connection;
use crate::{HttpBodyType, HttpHeaderLine, HttpLineParseError, HttpStatusLine};

pub struct HttpTransparentResponse {
    pub version: Version,
    pub code: u16,
    pub reason: String,
    pub end_to_end_headers: HttpHeaderMap,
    pub hop_by_hop_headers: HttpHeaderMap,
    original_connection_name: Connection,
    extra_connection_headers: Vec<HeaderName>,
    origin_header_size: usize,
    keep_alive: bool,
    connection_upgrade: bool,
    pub upgrade: Option<HttpUpgradeToken>,
    content_length: u64,
    chunked_transfer: bool,
    has_transfer_encoding: bool,
    has_content_length: bool,
    has_keep_alive: bool,
}

impl HttpTransparentResponse {
    fn new(version: Version, code: u16, reason: String) -> Self {
        HttpTransparentResponse {
            version,
            code,
            reason,
            end_to_end_headers: HttpHeaderMap::default(),
            hop_by_hop_headers: HttpHeaderMap::default(),
            original_connection_name: Connection::default(),
            extra_connection_headers: Vec::new(),
            origin_header_size: 0,
            keep_alive: false,
            connection_upgrade: false,
            upgrade: None,
            content_length: 0,
            chunked_transfer: false,
            has_transfer_encoding: false,
            has_content_length: false,
            has_keep_alive: false,
        }
    }

    pub fn adapt_with_body(&self, adapted: HttpAdaptedResponse) -> Self {
        let mut hop_by_hop_headers = self.hop_by_hop_headers.clone();
        match adapted.content_length {
            Some(content_length) => {
                hop_by_hop_headers.remove(header::TRANSFER_ENCODING);
                HttpTransparentResponse {
                    version: adapted.version,
                    code: adapted.status.as_u16(),
                    reason: adapted.reason,
                    end_to_end_headers: adapted.headers,
                    hop_by_hop_headers,
                    original_connection_name: self.original_connection_name.clone(),
                    extra_connection_headers: self.extra_connection_headers.clone(),
                    origin_header_size: self.origin_header_size,
                    keep_alive: self.keep_alive,
                    connection_upgrade: self.connection_upgrade,
                    upgrade: self.upgrade.clone(),
                    content_length,
                    chunked_transfer: false,
                    has_transfer_encoding: false,
                    has_content_length: true,
                    has_keep_alive: self.has_keep_alive,
                }
            }
            None => {
                if !self.chunked_transfer {
                    if let Some(mut v) = hop_by_hop_headers.remove(header::TRANSFER_ENCODING) {
                        v.set_static_value("chunked");
                        hop_by_hop_headers.insert(header::TRANSFER_ENCODING, v);
                    } else {
                        hop_by_hop_headers.insert(
                            header::TRANSFER_ENCODING,
                            HttpHeaderValue::from_static("chunked"),
                        );
                    }
                }
                HttpTransparentResponse {
                    version: adapted.version,
                    code: adapted.status.as_u16(),
                    reason: adapted.reason,
                    end_to_end_headers: adapted.headers,
                    hop_by_hop_headers,
                    original_connection_name: self.original_connection_name.clone(),
                    extra_connection_headers: self.extra_connection_headers.clone(),
                    origin_header_size: self.origin_header_size,
                    keep_alive: self.keep_alive,
                    connection_upgrade: self.connection_upgrade,
                    upgrade: self.upgrade.clone(),
                    content_length: 0,
                    chunked_transfer: true,
                    has_transfer_encoding: true,
                    has_content_length: false,
                    has_keep_alive: self.has_keep_alive,
                }
            }
        }
    }

    pub fn adapt_without_body(&self, adapted: HttpAdaptedResponse) -> Self {
        let mut hop_by_hop_headers = self.hop_by_hop_headers.clone();
        hop_by_hop_headers.remove(header::TRANSFER_ENCODING);
        let mut end_to_end_headers = adapted.headers;
        if let Some(mut v) = end_to_end_headers.remove(header::CONTENT_LENGTH) {
            v.set_static_value("0");
            end_to_end_headers.insert(header::CONTENT_LENGTH, v);
        } else {
            end_to_end_headers.insert(header::CONTENT_LENGTH, HttpHeaderValue::from_static("0"));
        }
        HttpTransparentResponse {
            version: adapted.version,
            code: adapted.status.as_u16(),
            reason: adapted.reason,
            end_to_end_headers,
            hop_by_hop_headers,
            original_connection_name: self.original_connection_name.clone(),
            extra_connection_headers: self.extra_connection_headers.clone(),
            origin_header_size: self.origin_header_size,
            keep_alive: self.keep_alive,
            connection_upgrade: self.connection_upgrade,
            upgrade: self.upgrade.clone(),
            content_length: 0,
            chunked_transfer: false,
            has_transfer_encoding: false,
            has_content_length: true,
            has_keep_alive: self.has_keep_alive,
        }
    }

    pub fn keep_alive(&self) -> bool {
        self.keep_alive
    }

    pub fn set_no_keep_alive(&mut self) {
        if self.has_keep_alive {
            self.hop_by_hop_headers
                .remove(HeaderName::from_static("keep-alive"));
            self.has_keep_alive = false;
        }
        self.keep_alive = false;
    }

    fn expect_no_body(&self, method: &Method) -> bool {
        self.code < 200 || self.code == 204 || self.code == 304 || method.eq(&Method::HEAD)
    }

    pub fn body_type(&self, method: &Method) -> Option<HttpBodyType> {
        // see https://tools.ietf.org/html/rfc7230#section-3.3.1 for the Transfer-Encoding
        // see https://tools.ietf.org/html/rfc7230#section-3.3.2 for the Content-Length
        // see https://datatracker.ietf.org/doc/html/rfc7230#section-3.3.3 for Message Body Length
        if self.expect_no_body(method) {
            None
        } else if self.chunked_transfer {
            Some(HttpBodyType::Chunked)
        } else if self.has_content_length {
            if self.content_length > 0 {
                Some(HttpBodyType::ContentLength(self.content_length))
            } else {
                None
            }
        } else {
            Some(HttpBodyType::ReadUntilEnd)
        }
    }

    pub async fn parse<R>(
        reader: &mut R,
        method: &Method,
        keep_alive: bool,
        max_header_size: usize,
    ) -> Result<(Self, Bytes), HttpResponseParseError>
    where
        R: AsyncBufRead + Unpin,
    {
        let mut head_bytes = BytesMut::with_capacity(4096);

        let (found, nr) = reader
            .limited_read_buf_until(b'\n', max_header_size, &mut head_bytes)
            .await?;
        if nr == 0 {
            return Err(HttpResponseParseError::RemoteClosed);
        }
        if !found {
            return if nr < max_header_size {
                Err(HttpResponseParseError::RemoteClosed)
            } else {
                Err(HttpResponseParseError::TooLargeHeader(max_header_size))
            };
        }

        let mut rsp = HttpTransparentResponse::build_from_status_line(head_bytes.as_ref())?;
        rsp.keep_alive = keep_alive;

        loop {
            let header_size = head_bytes.len();
            if header_size >= max_header_size {
                return Err(HttpResponseParseError::TooLargeHeader(max_header_size));
            }
            let max_len = max_header_size - header_size;
            let (found, nr) = reader
                .limited_read_buf_until(b'\n', max_len, &mut head_bytes)
                .await?;
            if nr == 0 {
                return Err(HttpResponseParseError::RemoteClosed);
            }
            if !found {
                return if nr < max_len {
                    Err(HttpResponseParseError::RemoteClosed)
                } else {
                    Err(HttpResponseParseError::TooLargeHeader(max_header_size))
                };
            }

            let line_buf = &head_bytes[header_size..];
            if (line_buf.len() == 1 && line_buf[0] == b'\n')
                || (line_buf.len() == 2 && line_buf[0] == b'\r' && line_buf[1] == b'\n')
            {
                // header end line
                break;
            }
            rsp.parse_header_line(line_buf)?;
        }

        rsp.origin_header_size = head_bytes.len();
        rsp.post_check_and_fix(method);
        Ok((rsp, head_bytes.freeze()))
    }

    /// do some necessary check and fix
    fn post_check_and_fix(&mut self, method: &Method) {
        if !self.chunked_transfer {
            if self.expect_no_body(method) {
                // ignore the check of content-length as body is unexpected
            } else if !self.has_content_length {
                // read to end and close the connection
                self.keep_alive = false;
            }
        }

        if !self.connection_upgrade {
            self.upgrade = None;
            self.hop_by_hop_headers.remove(header::UPGRADE);
        }

        // Don't move non-standard connection headers to hop-by-hop headers, as we don't support them
    }

    fn build_from_status_line(line_buf: &[u8]) -> Result<Self, HttpResponseParseError> {
        let rsp =
            HttpStatusLine::parse(line_buf).map_err(HttpResponseParseError::InvalidStatusLine)?;
        let version = match rsp.version {
            0 => Version::HTTP_10,
            1 => Version::HTTP_11,
            2 => return Err(HttpResponseParseError::InvalidVersion(Version::HTTP_2)),
            _ => unreachable!(),
        };

        Ok(HttpTransparentResponse::new(
            version,
            rsp.code,
            rsp.reason.to_string(),
        ))
    }

    fn parse_header_line(&mut self, line_buf: &[u8]) -> Result<(), HttpResponseParseError> {
        let header =
            HttpHeaderLine::parse(line_buf).map_err(HttpResponseParseError::InvalidHeaderLine)?;
        self.handle_header(header)
    }

    fn insert_hop_by_hop_header(
        &mut self,
        name: HeaderName,
        header: &HttpHeaderLine,
    ) -> Result<(), HttpResponseParseError> {
        let mut value = HttpHeaderValue::from_str(header.value).map_err(|_| {
            HttpResponseParseError::InvalidHeaderLine(HttpLineParseError::InvalidHeaderValue)
        })?;
        value.set_original_name(header.name);
        self.hop_by_hop_headers.append(name, value);
        Ok(())
    }

    fn handle_header(&mut self, header: HttpHeaderLine) -> Result<(), HttpResponseParseError> {
        let name = HeaderName::from_str(header.name).map_err(|_| {
            HttpResponseParseError::InvalidHeaderLine(HttpLineParseError::InvalidHeaderName)
        })?;

        match name.as_str() {
            "connection" | "proxy-connection" => {
                // proxy-connection is not standard, but at least curl use it
                for v in header.value.to_lowercase().as_str().split(',') {
                    if v.is_empty() {
                        continue;
                    }

                    match v.trim() {
                        "keep-alive" => {
                            // keep the original value from request
                        }
                        "close" => {
                            self.keep_alive = false;
                        }
                        "upgrade" => {
                            self.connection_upgrade = true;
                            self.extra_connection_headers.push(header::UPGRADE);
                        }
                        s => {
                            if let Ok(h) = HeaderName::from_str(s) {
                                self.extra_connection_headers.push(h);
                            }
                        }
                    }
                }

                self.original_connection_name = Connection::from_original(header.name);
                return Ok(());
            }
            "upgrade" => {
                let protocol = HttpUpgradeToken::from_str(header.value)?;
                self.upgrade = Some(protocol);
                return self.insert_hop_by_hop_header(name, &header);
            }
            "transfer-encoding" => {
                // it's a hop-by-hop option, but we just pass it
                self.has_transfer_encoding = true;
                if self.has_content_length {
                    // delete content-length
                    self.content_length = 0;
                }

                let v = header.value.to_lowercase();
                if v.ends_with("chunked") {
                    self.chunked_transfer = true;
                } else if v.contains("chunked") {
                    return Err(HttpResponseParseError::InvalidChunkedTransferEncoding);
                }
                return self.insert_hop_by_hop_header(name, &header);
            }
            "content-length" => {
                if self.has_transfer_encoding {
                    // ignore content-length
                    return Ok(());
                }

                let content_length = u64::from_str(header.value)
                    .map_err(|_| HttpResponseParseError::InvalidContentLength)?;

                if self.has_content_length && self.content_length != content_length {
                    return Err(HttpResponseParseError::InvalidContentLength);
                }
                self.has_content_length = true;
                self.content_length = content_length;
            }
            "proxy-authenticate" => return self.insert_hop_by_hop_header(name, &header),
            _ => {}
        }

        let mut value = HttpHeaderValue::from_str(header.value).map_err(|_| {
            HttpResponseParseError::InvalidHeaderLine(HttpLineParseError::InvalidHeaderValue)
        })?;
        value.set_original_name(header.name);
        self.end_to_end_headers.append(name, value);
        Ok(())
    }

    pub fn serialize(&self) -> Vec<u8> {
        const RESERVED_LEN_FOR_EXTRA_HEADERS: usize = 256;
        let mut buf =
            Vec::<u8>::with_capacity(self.origin_header_size + RESERVED_LEN_FOR_EXTRA_HEADERS);

        let _ = write!(buf, "{:?} {} {}\r\n", self.version, self.code, self.reason);

        self.end_to_end_headers
            .for_each(|name, value| value.write_to_buf(name, &mut buf));
        self.hop_by_hop_headers
            .for_each(|name, value| value.write_to_buf(name, &mut buf));
        self.original_connection_name.write_to_buf(
            !self.keep_alive,
            &self.extra_connection_headers,
            &mut buf,
        );
        buf.put_slice(b"\r\n");
        buf
    }

    pub fn serialize_for_adapter(&self) -> Vec<u8> {
        let mut buf = Vec::<u8>::with_capacity(self.origin_header_size);

        let _ = write!(buf, "{:?} {} {}\r\n", self.version, self.code, self.reason);

        self.end_to_end_headers
            .for_each(|name, value| value.write_to_buf(name, &mut buf));
        buf.put_slice(b"\r\n");
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn read_get() {
        let content = b"HTTP/1.1 200 OK\r\n\
            Date: Fri, 11 Nov 2022 03:22:03 GMT\r\n\
            Content-Type: text/plain; charset=utf-8\r\n\
            Content-Length: 4\r\n\
            Connection: keep-alive\r\n\r\n";
        let stream = tokio_test::io::Builder::new().read(content).build();
        let mut buf_stream = BufReader::new(stream);
        let method = Method::GET;
        let (rsp, data) = HttpTransparentResponse::parse(&mut buf_stream, &method, true, 4096)
            .await
            .unwrap();
        assert_eq!(data.as_ref(), content.as_slice());
        assert_eq!(rsp.code, 200);
        assert!(rsp.keep_alive());
        assert_eq!(rsp.body_type(&method), Some(HttpBodyType::ContentLength(4)));
    }

    #[tokio::test]
    async fn read_get_to_end() {
        let content = b"HTTP/1.1 200 OK\r\n\
            Date: Fri, 11 Nov 2022 03:22:03 GMT\r\n\
            Content-Type: text/plain; charset=utf-8\r\n\
            Connection: close\r\n\r\n";
        let stream = tokio_test::io::Builder::new().read(content).build();
        let mut buf_stream = BufReader::new(stream);
        let method = Method::GET;
        let (rsp, data) = HttpTransparentResponse::parse(&mut buf_stream, &method, true, 4096)
            .await
            .unwrap();
        assert_eq!(data.as_ref(), content.as_slice());
        assert_eq!(rsp.code, 200);
        assert!(!rsp.keep_alive());
        assert_eq!(rsp.body_type(&method), Some(HttpBodyType::ReadUntilEnd));
    }
}
