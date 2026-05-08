use crate::probe::{Probe, ProbeError, ProbeOutcome};
use socket2::{Domain, Protocol, Socket, Type};
use std::io;
use std::mem::MaybeUninit;
use std::net::{IpAddr, SocketAddr};
use std::time::Instant;

const ICMPV4_ECHO_REQUEST: u8 = 8;
const ICMPV4_ECHO_REPLY: u8 = 0;
const ICMPV6_ECHO_REQUEST: u8 = 128;
const ICMPV6_ECHO_REPLY: u8 = 129;

pub struct IcmpProbe {
    socket: Socket,
    target: IpAddr,
    ident: u16,
}

impl IcmpProbe {
    pub fn new(target: IpAddr) -> Result<Self, ProbeError> {
        let (domain, protocol) = match target {
            IpAddr::V4(_) => (Domain::IPV4, Protocol::ICMPV4),
            IpAddr::V6(_) => (Domain::IPV6, Protocol::ICMPV6),
        };
        let socket = Socket::new(domain, Type::DGRAM, Some(protocol)).map_err(|e| {
            match e.raw_os_error() {
                Some(c) if c == libc::EACCES || c == libc::EPERM => ProbeError {
                    message: format!(
                        "unable to open unprivileged ICMP socket ({}). \
                         On Linux, either widen `sysctl net.ipv4.ping_group_range` to include \
                         your GID, or grant CAP_NET_RAW via `sudo setcap cap_net_raw+ep <binary>`.",
                        e
                    ),
                },
                _ => ProbeError {
                    message: format!("failed to open ICMP socket: {}", e),
                },
            }
        })?;
        // The kernel rewrites the identifier on unprivileged ICMP sockets, so matching is
        // done on sequence number below. We still set an ident for privileged-socket paths.
        let ident = (std::process::id() & 0xFFFF) as u16;
        Ok(IcmpProbe {
            socket,
            target,
            ident,
        })
    }

    fn build_echo_request(&self, seq: u16) -> [u8; 16] {
        let (typ, is_v6) = match self.target {
            IpAddr::V4(_) => (ICMPV4_ECHO_REQUEST, false),
            IpAddr::V6(_) => (ICMPV6_ECHO_REQUEST, true),
        };
        let mut pkt = [0u8; 16];
        pkt[0] = typ;
        pkt[1] = 0;
        pkt[4..6].copy_from_slice(&self.ident.to_be_bytes());
        pkt[6..8].copy_from_slice(&seq.to_be_bytes());
        pkt[8..16].copy_from_slice(b"bidebide");
        if !is_v6 {
            let cs = checksum(&pkt);
            pkt[2..4].copy_from_slice(&cs.to_be_bytes());
        }
        // IPv6: kernel computes the ICMPv6 checksum (needs the v6 pseudo-header).
        pkt
    }

    fn is_matching_reply(&self, data: &[u8], seq: u16) -> bool {
        if data.len() < 8 {
            return false;
        }
        let expected_type = match self.target {
            IpAddr::V4(_) => ICMPV4_ECHO_REPLY,
            IpAddr::V6(_) => ICMPV6_ECHO_REPLY,
        };
        if data[0] != expected_type {
            return false;
        }
        let rseq = u16::from_be_bytes([data[6], data[7]]);
        // Identifier is rewritten by the kernel on unprivileged sockets. Since the socket
        // only receives replies addressed to it, sequence match is sufficient.
        rseq == seq
    }
}

impl Probe for IcmpProbe {
    fn target(&self) -> IpAddr {
        self.target
    }

    fn name(&self) -> &str {
        "icmp"
    }

    fn probe(&mut self, seq: u16, deadline: Instant) -> Result<ProbeOutcome, ProbeError> {
        let pkt = self.build_echo_request(seq);
        let dest: SocketAddr = SocketAddr::new(self.target, 0);
        let started = Instant::now();

        self.socket
            .send_to(&pkt, &dest.into())
            .map_err(|e| ProbeError {
                message: format!("send failed: {}", e),
            })?;

        loop {
            let now = Instant::now();
            if now >= deadline {
                return Ok(ProbeOutcome::NoResponse);
            }
            let remaining = deadline - now;
            self.socket
                .set_read_timeout(Some(remaining))
                .map_err(|e| ProbeError {
                    message: format!("setsockopt(SO_RCVTIMEO): {}", e),
                })?;

            let mut buf = [MaybeUninit::<u8>::uninit(); 1500];
            match self.socket.recv_from(&mut buf) {
                Ok((n, _from)) => {
                    let data: &[u8] =
                        unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, n) };
                    if self.is_matching_reply(data, seq) {
                        return Ok(ProbeOutcome::Success {
                            rtt: started.elapsed(),
                            seq,
                        });
                    }
                    // Stray packet (different seq, or echo request we sent being looped back
                    // on some configurations). Keep waiting until deadline.
                }
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::TimedOut
                        || e.kind() == io::ErrorKind::Interrupted =>
                {
                    // EINTR: a signal (e.g. SIGINT) interrupted recv. Treat as no-response;
                    // the scheduler will notice the signal flag on return and exit cleanly.
                    return Ok(ProbeOutcome::NoResponse);
                }
                Err(e) => {
                    return Err(ProbeError {
                        message: format!("recv failed: {}", e),
                    });
                }
            }
        }
    }
}

/// RFC 1071 one's-complement sum over 16-bit words.
fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum = sum.wrapping_add(u16::from_be_bytes([data[i], data[i + 1]]) as u32);
        i += 2;
    }
    if i < data.len() {
        sum = sum.wrapping_add((data[i] as u32) << 8);
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_known_value() {
        // RFC 1071 example: sum of 0x0001, 0xf203, 0xf4f5, 0xf6f7 = one's complement 0x220d.
        let data = [0x00u8, 0x01, 0xf2, 0x03, 0xf4, 0xf5, 0xf6, 0xf7];
        assert_eq!(checksum(&data), 0x220d);
    }

    #[test]
    fn echo_request_has_correct_type_v4() {
        let p = IcmpProbe {
            socket: Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::ICMPV4))
                .expect("needs unprivileged ICMP to run test"),
            target: IpAddr::from([127u8, 0, 0, 1]),
            ident: 0x1234,
        };
        let pkt = p.build_echo_request(7);
        assert_eq!(pkt[0], ICMPV4_ECHO_REQUEST);
        assert_eq!(pkt[1], 0);
        assert_eq!(u16::from_be_bytes([pkt[4], pkt[5]]), 0x1234);
        assert_eq!(u16::from_be_bytes([pkt[6], pkt[7]]), 7);
    }
}
