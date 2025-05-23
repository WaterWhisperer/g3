/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2024-2025 ByteDance and/or its affiliates.
 */

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures_util::Stream;
use hickory_proto::xfer::{DnsRequest, DnsRequestSender, DnsResponse, DnsResponseStream};
use hickory_proto::{ProtoError, ProtoErrorKind};
use quinn::{Connection, RecvStream, VarInt};
use rustls::ClientConfig;

use g3_socket::UdpConnectInfo;

pub async fn connect(
    connect_info: UdpConnectInfo,
    tls_config: ClientConfig,
    tls_name: String,
    connect_timeout: Duration,
    request_timeout: Duration,
) -> Result<QuicClientStream, ProtoError> {
    let connection = tokio::time::timeout(
        connect_timeout,
        crate::connect::quinn::quic_connect(connect_info, tls_config, &tls_name, b"doq"),
    )
    .await
    .map_err(|_| ProtoError::from("quic connect timed out"))??;
    Ok(QuicClientStream::new(connection, request_timeout))
}

/// A DNS client connection for DNS-over-QUIC
#[must_use = "futures do nothing unless polled"]
pub struct QuicClientStream {
    quic_connection: Connection,
    request_timeout: Duration,
    is_shutdown: bool,
}

impl QuicClientStream {
    pub fn new(connection: Connection, request_timeout: Duration) -> Self {
        QuicClientStream {
            quic_connection: connection,
            request_timeout,
            is_shutdown: false,
        }
    }
}

impl DnsRequestSender for QuicClientStream {
    /// The send loop for QUIC in DNS stipulates that a new QUIC "stream" should be opened and use for sending data.
    ///
    /// It should be closed after receiving the response. TODO: AXFR/IXFR support...
    fn send_message(&mut self, mut message: DnsRequest) -> DnsResponseStream {
        if self.is_shutdown {
            panic!("can not send messages after stream is shutdown")
        }

        // per the RFC, the DNS Message ID MUST be set to zero
        message.set_id(0);

        Box::pin(timed_quic_send_recv(
            self.quic_connection.clone(),
            message,
            self.request_timeout,
        ))
        .into()
    }

    fn shutdown(&mut self) {
        self.is_shutdown = true;
        // no error
        self.quic_connection.close(VarInt::from_u32(0), b"Shutdown");
    }

    fn is_shutdown(&self) -> bool {
        self.is_shutdown
    }
}

impl Stream for QuicClientStream {
    type Item = Result<(), ProtoError>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.is_shutdown {
            Poll::Ready(None)
        } else {
            Poll::Ready(Some(Ok(())))
        }
    }
}

async fn timed_quic_send_recv(
    connection: Connection,
    message: DnsRequest,
    request_timeout: Duration,
) -> Result<DnsResponse, ProtoError> {
    tokio::time::timeout(request_timeout, quic_send_recv(connection, message))
        .await
        .map_err(|_| ProtoErrorKind::Timeout)?
}

async fn quic_send_recv(
    connection: Connection,
    message: DnsRequest,
) -> Result<DnsResponse, ProtoError> {
    let message = message.into_parts().0;
    let (mut send_stream, recv_stream) = connection
        .open_bi()
        .await
        .map_err(|e| format!("quic open_bi error: {e}"))?;

    // prepare the buffer
    let buffer = Bytes::from(message.to_vec()?);
    let message_len = u16::try_from(buffer.len())
        .map_err(|_| ProtoErrorKind::MaxBufferSizeExceeded(buffer.len()))?;
    let len = Bytes::from(message_len.to_be_bytes().to_vec());

    send_stream
        .write_all_chunks(&mut [len, buffer])
        .await
        .map_err(|e| format!("quic write request error: {e}"))?;
    // The client MUST send the DNS query over the selected stream,
    // and MUST indicate through the STREAM FIN mechanism that no further data will be sent on that stream.
    send_stream
        .finish()
        .map_err(|e| format!("quic mark finish error: {e}"))?;

    quic_recv(recv_stream).await
}

async fn quic_recv(mut recv_stream: RecvStream) -> Result<DnsResponse, ProtoError> {
    let mut len_buf = [0u8; 2];
    recv_stream
        .read_exact(&mut len_buf)
        .await
        .map_err(|e| format!("quic read len error: {e}"))?;
    let message_len = u16::from_be_bytes(len_buf) as usize;

    let mut buffer = vec![0u8; message_len];
    recv_stream
        .read_exact(&mut buffer)
        .await
        .map_err(|e| format!("quic read message error: {e}"))?;
    let rsp = DnsResponse::from_buffer(buffer)?;
    if rsp.id() != 0 {
        return Err(ProtoError::from("quic response message id is not zero"));
    }

    Ok(rsp)
}
