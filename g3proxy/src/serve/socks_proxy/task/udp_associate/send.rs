/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2023-2025 ByteDance and/or its affiliates.
 */

use std::io::{self, IoSlice};
use std::net::SocketAddr;
use std::task::{Context, Poll, ready};

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "macos",
    target_os = "solaris",
))]
use g3_io_ext::UdpRelayPacket;
use g3_io_ext::{AsyncUdpSend, UdpRelayClientError, UdpRelayClientSend};
use g3_io_sys::udp::SendMsgHdr;
use g3_socks::v5::SocksUdpHeader;
use g3_types::net::UpstreamAddr;

pub(super) struct Socks5UdpAssociateClientSend<T> {
    inner: T,
    client: SocketAddr,
    socks_headers: Vec<SocksUdpHeader>,
}

impl<T> Socks5UdpAssociateClientSend<T>
where
    T: AsyncUdpSend,
{
    pub(super) fn new(inner: T, client: SocketAddr) -> Self {
        Socks5UdpAssociateClientSend {
            inner,
            client,
            socks_headers: vec![SocksUdpHeader::default(); 4],
        }
    }
}

impl<T> UdpRelayClientSend for Socks5UdpAssociateClientSend<T>
where
    T: AsyncUdpSend + Send,
{
    fn poll_send_packet(
        &mut self,
        cx: &mut Context<'_>,
        buf: &[u8],
        from: &UpstreamAddr,
    ) -> Poll<Result<usize, UdpRelayClientError>> {
        let socks_header = self.socks_headers.get_mut(0).unwrap();
        let hdr = SendMsgHdr::new(
            [IoSlice::new(socks_header.encode(from)), IoSlice::new(buf)],
            Some(self.client),
        );
        let nw =
            ready!(self.inner.poll_sendmsg(cx, &hdr)).map_err(UdpRelayClientError::SendFailed)?;
        if nw == 0 {
            Poll::Ready(Err(UdpRelayClientError::SendFailed(io::Error::new(
                io::ErrorKind::WriteZero,
                "write zero byte into sender",
            ))))
        } else {
            Poll::Ready(Ok(nw))
        }
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "solaris",
    ))]
    fn poll_send_packets(
        &mut self,
        cx: &mut Context<'_>,
        packets: &[UdpRelayPacket],
    ) -> Poll<Result<usize, UdpRelayClientError>> {
        if packets.len() > self.socks_headers.len() {
            self.socks_headers.resize(packets.len(), Default::default());
        }
        let mut msgs = Vec::with_capacity(packets.len());
        for (p, h) in packets.iter().zip(self.socks_headers.iter_mut()) {
            msgs.push(SendMsgHdr::new(
                [
                    IoSlice::new(h.encode(p.upstream())),
                    IoSlice::new(p.payload()),
                ],
                None,
            ));
        }

        let count = ready!(self.inner.poll_batch_sendmsg(cx, &mut msgs))
            .map_err(UdpRelayClientError::SendFailed)?;
        if count == 0 {
            Poll::Ready(Err(UdpRelayClientError::SendFailed(io::Error::new(
                io::ErrorKind::WriteZero,
                "write zero packet into sender",
            ))))
        } else {
            Poll::Ready(Ok(count))
        }
    }

    #[cfg(target_os = "macos")]
    fn poll_send_packets(
        &mut self,
        cx: &mut Context<'_>,
        packets: &[UdpRelayPacket],
    ) -> Poll<Result<usize, UdpRelayClientError>> {
        if packets.len() > self.socks_headers.len() {
            self.socks_headers.resize(packets.len(), Default::default());
        }
        let mut msgs = Vec::with_capacity(packets.len());
        for (p, h) in packets.iter().zip(self.socks_headers.iter_mut()) {
            msgs.push(SendMsgHdr::new(
                [
                    IoSlice::new(h.encode(p.upstream())),
                    IoSlice::new(p.payload()),
                ],
                None,
            ));
        }

        let count = ready!(self.inner.poll_batch_sendmsg_x(cx, &mut msgs))
            .map_err(UdpRelayClientError::SendFailed)?;
        if count == 0 {
            Poll::Ready(Err(UdpRelayClientError::SendFailed(io::Error::new(
                io::ErrorKind::WriteZero,
                "write zero packet into sender",
            ))))
        } else {
            Poll::Ready(Ok(count))
        }
    }
}
