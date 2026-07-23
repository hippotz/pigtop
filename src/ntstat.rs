//! Client for the private `com.apple.network.statistics` ("ntstat") kernel control socket —
//! the same interface Apple's own `nettop`/Activity Monitor use for per-process network stats.
//! Struct layouts are taken field-for-field from `bsd/net/ntstat.h` and `bsd/netinet/in_stat.h`
//! in apple-oss-distributions/xnu (fetched live during development), so repr(C) here reproduces
//! the kernel's layout exactly rather than relying on manually computed offsets.

use std::io;
use std::mem;
use std::os::unix::io::RawFd;
use std::time::Duration;

/// Baseline `SO_RCVTIMEO` used outside of boost mode — see `NtstatClient::set_recv_timeout`.
pub const DEFAULT_RECV_TIMEOUT: Duration = Duration::from_millis(50);

// --- PF_SYSTEM / kernel control socket setup (not exposed by the `libc` crate) ---

const PF_SYSTEM: i32 = 32;
const AF_SYSTEM: u8 = 32;
const AF_SYS_CONTROL: u16 = 2;
const SYSPROTO_CONTROL: i32 = 2;
const MAX_KCTL_NAME: usize = 96;

#[repr(C)]
struct CtlInfo {
    ctl_id: u32,
    ctl_name: [u8; MAX_KCTL_NAME],
}

#[repr(C)]
struct SockaddrCtl {
    sc_len: u8,
    sc_family: u8,
    ss_sysaddr: u16,
    sc_id: u32,
    sc_unit: u32,
    sc_reserved: [u32; 5],
}

const IOCPARM_MASK: u64 = 0x1fff;
const IOC_OUT: u64 = 0x4000_0000;
const IOC_IN: u64 = 0x8000_0000;
const IOC_INOUT: u64 = IOC_IN | IOC_OUT;

const fn ioc(inout: u64, group: u8, num: u8, len: usize) -> u64 {
    inout | ((len as u64 & IOCPARM_MASK) << 16) | ((group as u64) << 8) | (num as u64)
}

fn ctliocginfo() -> u64 {
    ioc(IOC_INOUT, b'N', 3, size_of::<CtlInfo>())
}

// --- ntstat wire protocol (bsd/net/ntstat.h, __NSTAT_REVISION__ 9) ---

const NSTAT_PROVIDER_TCP_KERNEL: u32 = 2;
const NSTAT_PROVIDER_TCP_USERLAND: u32 = 3;
const NSTAT_PROVIDER_UDP_KERNEL: u32 = 4;
const NSTAT_PROVIDER_UDP_USERLAND: u32 = 5;

const NSTAT_MSG_TYPE_ERROR: u32 = 1;
const NSTAT_MSG_TYPE_ADD_ALL_SRCS: u32 = 1002;
const NSTAT_MSG_TYPE_GET_UPDATE: u32 = 1007;
const NSTAT_MSG_TYPE_SRC_UPDATE: u32 = 10006;

const NSTAT_SRC_REF_ALL: u64 = 0xffff_ffff_ffff_ffff;

const NSTAT_MSG_HDR_FLAG_CONTINUATION: u16 = 1 << 1;

const NSTAT_MAX_MSG_SIZE: usize = 4096;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct NstatMsgHdr {
    context: u64,
    msg_type: u32,
    length: u16,
    flags: u16,
}

#[repr(C)]
struct NstatMsgAddAllSrcs {
    hdr: NstatMsgHdr,
    filter: u64,
    events: u64,
    provider: u32,
    target_pid: i32,
    target_uuid: [u8; 16],
}

#[repr(C)]
struct NstatMsgGetUpdate {
    hdr: NstatMsgHdr,
    srcref: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ActivityBitmap {
    start: u64,
    bitmap: [u64; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
union SockaddrInOrIn6 {
    v4: libc::sockaddr_in,
    v6: libc::sockaddr_in6,
}

// Field order/types below are copied verbatim from `nstat_tcp_descriptor` / `nstat_udp_descriptor`
// in ntstat.h so that repr(C) computes the same offsets the kernel used when writing the response.
#[repr(C)]
#[derive(Clone, Copy)]
struct NstatTcpDescriptor {
    upid: u64,
    eupid: u64,
    start_timestamp: u64,
    timestamp: u64,
    rx_transfer_size: u64,
    tx_transfer_size: u64,
    activity_bitmap: ActivityBitmap,
    ifindex: u32,
    state: u32,
    sndbufsize: u32,
    sndbufused: u32,
    rcvbufsize: u32,
    rcvbufused: u32,
    txunacked: u32,
    txwindow: u32,
    txcwindow: u32,
    traffic_class: u32,
    traffic_mgt_flags: u32,
    pid: u32,
    epid: u32,
    local: SockaddrInOrIn6,
    remote: SockaddrInOrIn6,
    cc_algo: [u8; 16],
    pname: [u8; 64],
    uuid: [u8; 16],
    euuid: [u8; 16],
    vuuid: [u8; 16],
    fuuid: [u8; 16],
    persona_id: u32,
    uid: u32,
    connstatus_pad: [u8; 4],
    ifnet_properties: u32,
    fallback_mode: u8,
    reserved: [u8; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NstatUdpDescriptor {
    upid: u64,
    eupid: u64,
    start_timestamp: u64,
    timestamp: u64,
    activity_bitmap: ActivityBitmap,
    local: SockaddrInOrIn6,
    remote: SockaddrInOrIn6,
    ifindex: u32,
    rcvbufsize: u32,
    rcvbufused: u32,
    traffic_class: u32,
    pid: u32,
    pname: [u8; 64],
    epid: u32,
    uuid: [u8; 16],
    euuid: [u8; 16],
    vuuid: [u8; 16],
    fuuid: [u8; 16],
    persona_id: u32,
    uid: u32,
    ifnet_properties: u32,
    fallback_mode: u8,
    reserved: [u8; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NstatCounts {
    rxpackets: u64,
    rxbytes: u64,
    txpackets: u64,
    txbytes: u64,
    cell_rxbytes: u64,
    cell_txbytes: u64,
    wifi_rxbytes: u64,
    wifi_txbytes: u64,
    wired_rxbytes: u64,
    wired_txbytes: u64,
    rxduplicatebytes: u32,
    rxoutoforderbytes: u32,
    txretransmit: u32,
    connectattempts: u32,
    connectsuccesses: u32,
    min_rtt: u32,
    avg_rtt: u32,
    var_rtt: u32,
}

// NSTAT_SRC_UPDATE_FIELDS: hdr, srcref, event_flags, counts, provider, reserved[4] — then variable descriptor data.
#[repr(C)]
#[derive(Clone, Copy)]
struct NstatMsgSrcUpdateHeader {
    hdr: NstatMsgHdr,
    srcref: u64,
    event_flags: u64,
    counts: NstatCounts,
    provider: u32,
    reserved: [u8; 4],
}

/// One source's data from a single poll: a TCP or UDP flow attributed to a process.
#[derive(Debug, Clone)]
pub struct SourceSample {
    pub srcref: u64,
    pub pid: u32,
    pub epid: u32,
    pub pname: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

pub struct NtstatClient {
    fd: RawFd,
    next_context: u64,
}

fn io_err(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Other, format!("{msg}: {}", io::Error::last_os_error()))
}

fn set_recv_timeout_on(fd: RawFd, timeout: Duration) -> io::Result<()> {
    unsafe {
        let tv =
            libc::timeval { tv_sec: timeout.as_secs() as _, tv_usec: timeout.subsec_micros() as _ };
        if libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            &tv as *const libc::timeval as *const libc::c_void,
            size_of::<libc::timeval>() as u32,
        ) != 0
        {
            return Err(io_err("setsockopt(SO_RCVTIMEO)"));
        }
    }
    Ok(())
}

impl NtstatClient {
    /// Opens and connects the ntstat kernel control socket. Works for a normal (non-root) user
    /// observing their own processes' sources.
    pub fn connect() -> io::Result<Self> {
        unsafe {
            let fd = libc::socket(PF_SYSTEM, libc::SOCK_DGRAM, SYSPROTO_CONTROL);
            if fd < 0 {
                return Err(io_err("socket(PF_SYSTEM, SOCK_DGRAM, SYSPROTO_CONTROL)"));
            }

            let mut ctl_info: CtlInfo = mem::zeroed();
            let name = b"com.apple.network.statistics\0";
            ctl_info.ctl_name[..name.len()].copy_from_slice(name);

            if libc::ioctl(fd, ctliocginfo(), &mut ctl_info as *mut CtlInfo) != 0 {
                libc::close(fd);
                return Err(io_err("ioctl(CTLIOCGINFO)"));
            }

            let mut addr: SockaddrCtl = mem::zeroed();
            addr.sc_len = size_of::<SockaddrCtl>() as u8;
            addr.sc_family = AF_SYSTEM;
            addr.ss_sysaddr = AF_SYS_CONTROL;
            addr.sc_id = ctl_info.ctl_id;
            addr.sc_unit = 0;

            if libc::connect(
                fd,
                &addr as *const SockaddrCtl as *const libc::sockaddr,
                size_of::<SockaddrCtl>() as u32,
            ) != 0
            {
                libc::close(fd);
                return Err(io_err("connect(AF_SYSTEM/com.apple.network.statistics)"));
            }

            // This is also the floor latency of every poll_all() call (the last recv() always
            // runs out the clock to detect "no more data"), so the caller's poll interval needs
            // to stay well above it or the interval is a no-op.
            if let Err(e) = set_recv_timeout_on(fd, DEFAULT_RECV_TIMEOUT) {
                libc::close(fd);
                return Err(e);
            }

            Ok(NtstatClient { fd, next_context: 1 })
        }
    }

    /// Bounds how long a single poll_all() can block waiting for the kernel to keep streaming
    /// fragments, so a protocol misunderstanding degrades to "returns early" rather than hanging
    /// forever. Callers driving a faster poll cadence (e.g. a UI "boost" mode) should shrink this
    /// to match, since every poll_all() call blocks for close to this long on its final recv().
    pub fn set_recv_timeout(&self, timeout: Duration) -> io::Result<()> {
        set_recv_timeout_on(self.fd, timeout)
    }

    fn send(&self, buf: &[u8]) -> io::Result<()> {
        unsafe {
            let n = libc::send(self.fd, buf.as_ptr() as *const libc::c_void, buf.len(), 0);
            if n < 0 {
                return Err(io_err("send"));
            }
        }
        Ok(())
    }

    fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        unsafe {
            let n = libc::recv(self.fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0);
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(n as usize)
        }
    }

    fn next_context(&mut self) -> u64 {
        let c = self.next_context;
        self.next_context += 1;
        c
    }

    /// Subscribes this client to every existing+future source of the given provider
    /// (drains and discards the resulting SRC_ADDED backlog — poll_all() re-derives the
    /// live source set from NSTAT_SRC_REF_ALL each time rather than tracking srcrefs here).
    fn add_all_sources(&mut self, provider: u32) -> io::Result<()> {
        let req = NstatMsgAddAllSrcs {
            hdr: NstatMsgHdr {
                context: self.next_context(),
                msg_type: NSTAT_MSG_TYPE_ADD_ALL_SRCS,
                length: size_of::<NstatMsgAddAllSrcs>() as u16,
                flags: 0,
            },
            filter: 0,
            events: 0,
            provider,
            target_pid: 0,
            target_uuid: [0; 16],
        };
        let bytes = unsafe {
            std::slice::from_raw_parts(
                &req as *const NstatMsgAddAllSrcs as *const u8,
                size_of::<NstatMsgAddAllSrcs>(),
            )
        };
        self.send(bytes)?;

        // Drain whatever backlog of SRC_ADDED/ERROR messages comes back immediately; we don't
        // need the srcrefs (poll_all uses NSTAT_SRC_REF_ALL), just need them off the socket.
        let mut buf = vec![0u8; NSTAT_MAX_MSG_SIZE];
        loop {
            match self.recv(&mut buf) {
                Ok(0) => break,
                Ok(_) => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    pub fn subscribe_tcp_udp(&mut self) -> io::Result<()> {
        for provider in [
            NSTAT_PROVIDER_TCP_KERNEL,
            NSTAT_PROVIDER_TCP_USERLAND,
            NSTAT_PROVIDER_UDP_KERNEL,
            NSTAT_PROVIDER_UDP_USERLAND,
        ] {
            self.add_all_sources(provider)?;
        }
        Ok(())
    }

    /// Polls the kernel for a fresh counts+descriptor snapshot of every currently subscribed
    /// TCP/UDP source (NSTAT_SRC_REF_ALL), following NSTAT_MSG_HDR_FLAG_CONTINUATION fragments
    /// until the kernel signals it's done, or until responses stop arriving.
    pub fn poll_all(&mut self) -> io::Result<Vec<SourceSample>> {
        let mut out = Vec::new();
        let mut buf = vec![0u8; NSTAT_MAX_MSG_SIZE];
        let context = self.next_context();
        let mut want_more = true;

        while want_more {
            want_more = false;

            let req = NstatMsgGetUpdate {
                hdr: NstatMsgHdr {
                    context,
                    msg_type: NSTAT_MSG_TYPE_GET_UPDATE,
                    length: size_of::<NstatMsgGetUpdate>() as u16,
                    flags: 0,
                },
                srcref: NSTAT_SRC_REF_ALL,
            };
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    &req as *const NstatMsgGetUpdate as *const u8,
                    size_of::<NstatMsgGetUpdate>(),
                )
            };
            self.send(bytes)?;

            // Each recv() is one aggregate datagram that may itself contain several
            // back-to-back sub-messages (NSTAT_MSG_HDR_FLAG_SUPPORTS_AGGREGATE); keep reading
            // datagrams until the kernel goes quiet for this poll round.
            loop {
                let n = match self.recv(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e),
                };

                let mut offset = 0usize;
                while offset + size_of::<NstatMsgHdr>() <= n {
                    // Sub-messages aren't guaranteed 8-byte aligned within the buffer, so read
                    // via read_unaligned rather than dereferencing a cast pointer directly.
                    let hdr = unsafe {
                        (buf[offset..].as_ptr() as *const NstatMsgHdr).read_unaligned()
                    };
                    let msg_len = hdr.length as usize;
                    if msg_len < size_of::<NstatMsgHdr>() || offset + msg_len > n {
                        break;
                    }
                    let msg = &buf[offset..offset + msg_len];

                    if hdr.flags & NSTAT_MSG_HDR_FLAG_CONTINUATION != 0 {
                        want_more = true;
                    }

                    match hdr.msg_type {
                        NSTAT_MSG_TYPE_SRC_UPDATE => {
                            if let Some(sample) = parse_src_update(msg) {
                                out.push(sample);
                            }
                        }
                        NSTAT_MSG_TYPE_ERROR if msg_len >= 20 => {
                            let error = u32::from_ne_bytes(msg[16..20].try_into().unwrap());
                            eprintln!("ntstat: kernel returned NSTAT_MSG_TYPE_ERROR errno={error}");
                        }
                        _ => {}
                    }

                    offset += msg_len;
                }
            }
        }

        Ok(out)
    }
}

impl Drop for NtstatClient {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}

fn cstr_bytes_to_string(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn parse_src_update(msg: &[u8]) -> Option<SourceSample> {
    let hdr_size = size_of::<NstatMsgSrcUpdateHeader>();
    if msg.len() < hdr_size {
        return None;
    }
    let hdr =
        unsafe { (msg.as_ptr() as *const NstatMsgSrcUpdateHeader).read_unaligned() };
    let data = &msg[hdr_size..];

    let (pid, epid, pname) = match hdr.provider {
        NSTAT_PROVIDER_TCP_KERNEL | NSTAT_PROVIDER_TCP_USERLAND => {
            if data.len() < size_of::<NstatTcpDescriptor>() {
                return None;
            }
            let d = unsafe { (data.as_ptr() as *const NstatTcpDescriptor).read_unaligned() };
            (d.pid, d.epid, cstr_bytes_to_string(&d.pname))
        }
        NSTAT_PROVIDER_UDP_KERNEL | NSTAT_PROVIDER_UDP_USERLAND => {
            if data.len() < size_of::<NstatUdpDescriptor>() {
                return None;
            }
            let d = unsafe { (data.as_ptr() as *const NstatUdpDescriptor).read_unaligned() };
            (d.pid, d.epid, cstr_bytes_to_string(&d.pname))
        }
        _ => return None,
    };

    Some(SourceSample {
        srcref: hdr.srcref,
        pid,
        epid,
        pname,
        rx_bytes: hdr.counts.rxbytes,
        tx_bytes: hdr.counts.txbytes,
    })
}
